//! `wn setup` (alias `wn skill sync`): connects Claude Code, Codex and Cursor to wn. Per agent it
//! installs the where-next skill (`SKILL.md`) and hooks that add wn's hints to the agent's context
//! (see [`crate::hooks`]), keeps both current, and removes them again with `--uninstall`.
//!
//! | agent  | skill (user scope; `--project`: same under the repository) | hooks                     |
//! |--------|--------------------------------------------------------------|---------------------------|
//! | claude | `~/.claude/skills/where-next/SKILL.md`                       | `~/.claude/settings.json` |
//! | codex  | `~/.agents/skills/where-next/SKILL.md`                       | `~/.codex/hooks.json`     |
//! | cursor | `~/.cursor/skills/where-next/SKILL.md`                       | `~/.cursor/hooks.json`    |
//!
//! Only files carrying the managed marker are updated or removed; anything else at a skill target
//! is reported as a conflict and left alone. Hook entries are recognised by their command
//! (`… wn hook <agent>-<moment>`); everything else in a settings file is kept as it is, and a
//! file that is not valid JSON is left alone. Installed targets are recorded in
//! `$WHERE_NEXT_HOME/skills.json` so `wn update` can re-sync them (`--from-state`).
//!
//! The sync itself is an explicit state machine ([`SyncState`]): plan, review (diff + confirm),
//! apply.

use std::fmt;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use clap::{Args, Subcommand, ValueEnum};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hooks::HookKind;

/// The skill source, compiled in so `wn setup` works without a checkout.
pub const SKILL: &str = include_str!("../assets/where-next/SKILL.md");

/// Marker line identifying files this command manages.
pub const MARKER: &str = "<!-- managed by `wn skill sync`";

/// Seconds an agent waits for one of our hooks before giving up (the hook itself answers within
/// [`crate::hooks::DEFAULT_BUDGET_MS`] or prints nothing).
pub const HOOK_TIMEOUT_S: u64 = 5;

/// `wn skill …`
#[derive(Debug, Clone, Subcommand)]
pub enum SkillAction {
    /// Same as `wn setup`: install or update the where-next skill and hooks for coding agents.
    #[command(after_help = SETUP_EXAMPLES)]
    Sync(SyncArgs),
    /// Print the skill.
    Show,
}

/// Options of `wn setup` / `wn skill sync`.
#[derive(Debug, Clone, Args)]
pub struct SyncArgs {
    /// Agents to connect (default: those detected in your home directory).
    #[arg(long = "agent", value_enum)]
    pub agents: Vec<AgentArg>,
    /// Install into this repository (`.claude/`, `.agents/`, `.codex/`, `.cursor/`) instead of
    /// your home directory.
    #[arg(long)]
    pub project: bool,
    /// Show what would change without writing anything.
    #[arg(long)]
    pub dry_run: bool,
    /// Remove the skill and hooks instead.
    #[arg(long)]
    pub uninstall: bool,
    /// Apply without asking.
    #[arg(long, short)]
    pub yes: bool,
    /// Only the skill: no hooks.
    #[arg(long)]
    pub no_hooks: bool,
    /// Re-sync exactly the targets recorded by earlier runs (used by `wn update`).
    #[arg(long)]
    pub from_state: bool,
    /// Accepted for compatibility: hooks are installed by default now.
    #[arg(long, hide = true)]
    pub with_hook: bool,
}

pub const SETUP_EXAMPLES: &str = "\
Installs, per detected agent, the where-next skill and hooks that add wn's hints to the agent's
context: on every prompt (Claude Code, Codex) and after a search that found nothing or too much
(rg/grep/find/fd, Grep/Glob tools). Shows every file it will write and asks first. Hooks never
block: they answer within 1.5 s or print nothing, and stay silent when wn is unsure, the repository
is not indexed, or WN_HOOKS=0. `wn stats` shows what they did.

Examples:
  wn setup --dry-run                       show what would change, write nothing
  wn setup                                 agents detected in your home directory
  wn setup --agent claude --yes
  wn setup --no-hooks                      only the skill
  wn setup --project                       this repository's .claude/.agents/.codex/.cursor
  wn setup --uninstall                     remove everything wn setup added";

/// `--agent` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum AgentArg {
    Claude,
    Codex,
    Cursor,
    All,
}

/// A coding agent that reads `SKILL.md` files.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum Agent {
    Claude,
    Codex,
    Cursor,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Cursor];

    fn dir_name(self) -> &'static str {
        match self {
            Agent::Claude => ".claude",
            Agent::Codex => ".agents",
            Agent::Cursor => ".cursor",
        }
    }

    /// Skill directory for this agent under `base` (a home or repository root).
    pub fn skill_dir(self, base: &Path) -> PathBuf {
        base.join(self.dir_name()).join("skills").join("where-next")
    }

    /// The file holding this agent's hooks under `base`.
    pub fn hook_file(self, base: &Path) -> PathBuf {
        match self {
            Agent::Claude => base.join(".claude").join("settings.json"),
            Agent::Codex => base.join(".codex").join("hooks.json"),
            Agent::Cursor => base.join(".cursor").join("hooks.json"),
        }
    }

    /// Whether the agent looks installed for the user whose home is `home`.
    pub fn detected(self, home: &Path) -> bool {
        match self {
            Agent::Claude => home.join(".claude").is_dir(),
            Agent::Codex => home.join(".codex").is_dir() || home.join(".agents").is_dir(),
            Agent::Cursor => home.join(".cursor").is_dir(),
        }
    }

    /// Display name.
    pub fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Cursor => "Cursor",
        }
    }
}

impl fmt::Display for Agent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Cursor => "cursor",
        })
    }
}

/// Version tag of the compiled-in skill: crate version plus a hash of its text.
pub fn skill_version() -> String {
    let mut h: u64 = 1469598103934665603;
    for b in SKILL.bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    format!("{}-{:08x}", env!("CARGO_PKG_VERSION"), h as u32)
}

/// The file written to disk: the skill with the managed marker right after its frontmatter
/// (agents require the frontmatter to come first).
pub fn rendered() -> String {
    let marker = format!(
        "{MARKER} {}; edits here are overwritten, see https://github.com/andreylukin/where-next -->",
        skill_version()
    );
    match SKILL
        .strip_prefix("---\n")
        .and_then(|rest| rest.find("\n---\n").map(|i| (rest, i)))
    {
        Some((rest, i)) => {
            let (front, body) = rest.split_at(i + "\n---\n".len());
            format!("---\n{front}{marker}\n{body}")
        }
        None => format!("{marker}\n{SKILL}"),
    }
}

/// One install location.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct Target {
    pub agent: Agent,
    /// `user` or `project`.
    pub scope: String,
    /// The `SKILL.md` path.
    pub path: PathBuf,
}

/// What happens to one target.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Create,
    Update {
        diff: String,
    },
    Unchanged,
    /// A file we did not write sits at the target; it is left alone.
    Conflict,
    Remove,
    /// Nothing of ours to remove.
    Absent,
}

// ---------------------------------------------------------------------------------------------
// Hook entries in agent settings
// ---------------------------------------------------------------------------------------------

/// One hook handler we install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HookEntry {
    /// The agent's event name.
    pub event: &'static str,
    /// Tool-name matcher, when the event takes one.
    pub matcher: Option<&'static str>,
    pub kind: HookKind,
}

/// The hooks installed for an agent. Claude Code and Codex share the `settings.json` hooks shape
/// (`hooks.<Event>[].hooks[]`); Cursor uses `hooks.json` version 1 (`hooks.<event>[]`).
/// Every agent gets a session-start warm-up. Cursor's prompt hook (`beforeSubmitPrompt`) cannot
/// add context, so Cursor gets no prompt hook.
pub fn hook_entries(agent: Agent) -> &'static [HookEntry] {
    const CLAUDE: &[HookEntry] = &[
        // Warm-up: starts the daemon so the first prompt's hook does not wait for the model.
        HookEntry {
            event: "SessionStart",
            matcher: None,
            kind: HookKind::ClaudeStart,
        },
        HookEntry {
            event: "UserPromptSubmit",
            matcher: None,
            kind: HookKind::ClaudePrompt,
        },
        HookEntry {
            event: "PostToolUse",
            matcher: Some("Grep|Glob|Bash"),
            kind: HookKind::ClaudeSearch,
        },
        // `rg`/`grep` exit 1 when nothing matches, which Claude Code reports as a failure.
        HookEntry {
            event: "PostToolUseFailure",
            matcher: Some("Bash"),
            kind: HookKind::ClaudeSearch,
        },
    ];
    const CODEX: &[HookEntry] = &[
        HookEntry {
            event: "SessionStart",
            matcher: None,
            kind: HookKind::CodexStart,
        },
        HookEntry {
            event: "UserPromptSubmit",
            matcher: None,
            kind: HookKind::CodexPrompt,
        },
        HookEntry {
            event: "PostToolUse",
            matcher: Some("Bash"),
            kind: HookKind::CodexSearch,
        },
    ];
    const CURSOR: &[HookEntry] = &[
        HookEntry {
            event: "sessionStart",
            matcher: None,
            kind: HookKind::CursorStart,
        },
        HookEntry {
            event: "postToolUse",
            matcher: Some("Shell|Grep"),
            kind: HookKind::CursorSearch,
        },
    ];
    match agent {
        Agent::Claude => CLAUDE,
        Agent::Codex => CODEX,
        Agent::Cursor => CURSOR,
    }
}

fn our_command_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r#"(?:^|[\s/'"])wn['"]?\s+hook\s+(?:claude|codex|cursor)-(?:prompt|search|start)\s*$"#,
        )
        .expect("valid regex")
    })
}

/// Whether a hook command is one `wn setup` installed (any `wn` path, any of our hook names).
pub fn is_our_command(command: &str) -> bool {
    our_command_re().is_match(command.trim())
}

/// How hook commands name the binary: `wn` when that is this binary on `PATH`, else its absolute
/// path (agents started outside a login shell may not have `wn` on `PATH`).
pub fn hook_program() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "wn".into();
    };
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let on_path = std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join("wn"))
            .find(|p| p.is_file())
    });
    if on_path.is_some_and(|p| canon(&p) == canon(&exe)) {
        return "wn".into();
    }
    let path = exe.to_string_lossy().into_owned();
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c))
    {
        path
    } else {
        format!("'{}'", path.replace('\'', r"'\''"))
    }
}

/// The command a hook entry runs.
pub fn hook_command(program: &str, kind: HookKind) -> String {
    format!("{program} hook {}", kind.name())
}

fn remove_ours(list: &mut Vec<Value>, nested: bool) -> bool {
    let before = list.clone();
    if nested {
        let mut emptied = vec![false; list.len()];
        for (i, entry) in list.iter_mut().enumerate() {
            if let Some(hs) = entry.get_mut("hooks").and_then(Value::as_array_mut) {
                let n = hs.len();
                hs.retain(|h| {
                    !h.get("command")
                        .and_then(Value::as_str)
                        .is_some_and(is_our_command)
                });
                emptied[i] = n > 0 && hs.is_empty();
            }
        }
        let mut i = 0;
        list.retain(|_| {
            let keep = !emptied[i];
            i += 1;
            keep
        });
    } else {
        list.retain(|h| {
            !h.get("command")
                .and_then(Value::as_str)
                .is_some_and(is_our_command)
        });
    }
    *list != before
}

/// The settings entry for one of our hooks.
fn entry_value(agent: Agent, e: &HookEntry, program: &str) -> Value {
    let command = hook_command(program, e.kind);
    let mut entry = serde_json::Map::new();
    if agent != Agent::Cursor {
        if let Some(m) = e.matcher {
            entry.insert("matcher".into(), Value::from(m));
        }
        entry.insert(
            "hooks".into(),
            serde_json::json!([{
                "type": "command",
                "command": command,
                "timeout": HOOK_TIMEOUT_S,
            }]),
        );
    } else {
        entry.insert("command".into(), Value::from(command));
        if let Some(m) = e.matcher {
            entry.insert("matcher".into(), Value::from(m));
        }
        entry.insert("timeout".into(), Value::from(HOOK_TIMEOUT_S));
    }
    Value::Object(entry)
}

/// Whether a settings entry is entirely ours.
fn is_our_entry(v: &Value, agent: Agent) -> bool {
    let ours = |h: &Value| {
        h.get("command")
            .and_then(Value::as_str)
            .is_some_and(is_our_command)
    };
    if agent == Agent::Cursor {
        return ours(v);
    }
    v.get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hs| !hs.is_empty() && hs.iter().all(ours))
}

/// Text-level removal of our entries (see [`crate::jsonedit`]). `None` when the text cannot be
/// scanned.
fn remove_text(text: &str, agent: Agent) -> Option<String> {
    use crate::jsonedit::{elements, member_items, members, remove_item, root};
    let mut t = text.to_string();
    loop {
        let b = t.as_bytes();
        let open = root(&t)?;
        let (rm, rclose) = members(b, open)?;
        let Some(hi) = rm.iter().position(|m| m.key == "hooks") else {
            return Some(t);
        };
        let hv = rm[hi].value;
        if b[hv.start] != b'{' {
            return Some(t);
        }
        let (hm, hclose) = members(b, hv.start)?;
        let mut next = None;
        'events: for (j, m) in hm.iter().enumerate() {
            if b[m.value.start] != b'[' {
                continue;
            }
            let (es, close) = elements(b, m.value.start)?;
            for (k, e) in es.iter().enumerate() {
                let v: Value = serde_json::from_str(&t[e.start..e.end]).ok()?;
                if !is_our_entry(&v, agent) {
                    continue;
                }
                next = Some(if es.len() > 1 {
                    remove_item(&t, m.value.start, close, &es, k)
                } else if hm.len() > 1 {
                    remove_item(&t, hv.start, hclose, &member_items(&hm), j)
                } else {
                    remove_item(&t, open, rclose, &member_items(&rm), hi)
                });
                break 'events;
            }
        }
        match next {
            Some(n) => t = n,
            None => return Some(t),
        }
    }
}

/// Text-level insertion of our entries. `None` when the text cannot be scanned.
fn add_text(text: &str, agent: Agent, program: &str) -> Option<String> {
    use crate::jsonedit::{insert_member, members, push_element, root};
    let mut t = text.to_string();
    for e in hook_entries(agent) {
        let entry = entry_value(agent, e, program);
        let b = t.as_bytes();
        let open = root(&t)?;
        let (rm, _) = members(b, open)?;
        t = match rm.iter().find(|m| m.key == "hooks") {
            None => insert_member(&t, open, "hooks", &serde_json::json!({ e.event: [entry] }))?,
            Some(h) if b[h.value.start] == b'{' => {
                let (hm, _) = members(b, h.value.start)?;
                match hm.iter().find(|m| m.key == e.event) {
                    Some(m) if b[m.value.start] == b'[' => push_element(&t, m.value.start, &entry)?,
                    Some(_) => return None,
                    None => insert_member(&t, h.value.start, e.event, &serde_json::json!([entry]))?,
                }
            }
            Some(_) => return None,
        };
    }
    Some(t)
}

/// Adds (`add`) or removes our hook entries for `agent` in a settings value; other content is
/// untouched. Removing drops only entries, event lists and the `hooks` object that our removal
/// left empty.
pub fn edit_hooks(settings: &mut Value, agent: Agent, program: &str, add: bool) {
    let Some(obj) = settings.as_object_mut() else {
        return;
    };
    let nested = agent != Agent::Cursor;
    let had_hooks = obj.contains_key("hooks");
    if !had_hooks && !add {
        return;
    }
    // A new Cursor hooks.json needs `version: 1`; an existing file keeps what it has.
    if add && agent == Agent::Cursor && obj.is_empty() {
        obj.insert("version".into(), Value::from(1));
    }
    let hooks = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(Default::default()));
    let Some(hooks) = hooks.as_object_mut() else {
        return;
    };
    let mut touched_events = Vec::new();
    for (event, list) in hooks.iter_mut() {
        if let Some(list) = list.as_array_mut() {
            if remove_ours(list, nested) && list.is_empty() {
                touched_events.push(event.clone());
            }
        }
    }
    let removed_any = !touched_events.is_empty();
    if add {
        for e in hook_entries(agent) {
            let entry = entry_value(agent, e, program);
            let list = hooks
                .entry(e.event)
                .or_insert_with(|| Value::Array(Vec::new()));
            if let Some(list) = list.as_array_mut() {
                list.push(entry);
            }
        }
    }
    // Event lists our removal emptied (and adding did not refill) go; keys keep their order.
    for event in &touched_events {
        if hooks
            .get(event)
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            hooks.shift_remove(event);
        }
    }
    let empty = hooks.is_empty();
    if empty && (removed_any || !had_hooks) {
        obj.shift_remove("hooks");
    }
}

/// Whether a settings text already holds hook entries of ours.
fn has_our_entries(text: &str) -> bool {
    serde_json::from_str::<Value>(text).is_ok_and(|v| {
        v.get("hooks")
            .and_then(Value::as_object)
            .is_some_and(|hooks| {
                hooks
                    .values()
                    .filter_map(Value::as_array)
                    .flatten()
                    .any(|e| {
                        let direct = e.get("command").and_then(Value::as_str);
                        let nested = e
                            .get("hooks")
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter_map(|h| h.get("command").and_then(Value::as_str));
                        direct.into_iter().chain(nested).any(is_our_command)
                    })
            })
    })
}

/// Whether any agent has where-next hooks, in the user's settings or in the repository at `root`.
pub fn connected(home: &Path, root: Option<&Path>) -> bool {
    Agent::ALL.iter().any(|a| {
        std::iter::once(home).chain(root).any(|base| {
            std::fs::read_to_string(a.hook_file(base)).is_ok_and(|t| has_our_entries(&t))
        })
    })
}

/// The one-line nudge shown where hooks would help but none are installed.
pub const CONNECT_HINT: &str = "Connect your agents so they get hints automatically: wn setup";

/// Whether a settings value holds nothing but what an empty file of this kind would.
fn is_bare(v: &Value) -> bool {
    v.as_object().is_some_and(|o| {
        o.iter()
            .all(|(k, val)| k == "version" && val == &Value::from(1))
    })
}

/// Whether Codex's `config.toml` next to `hooks.json` defines hooks inline (Codex warns when a
/// layer has both, so we leave such a layer alone).
fn codex_inline_hooks(hooks_json: &Path) -> bool {
    let toml = hooks_json.with_file_name("config.toml");
    std::fs::read_to_string(toml).is_ok_and(|t| {
        t.lines().any(|l| {
            let l = l.trim_start();
            l.starts_with("[hooks") || l.starts_with("[[hooks")
        })
    })
}

/// What happens to one agent's hook file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookAction {
    /// A new file.
    Create { diff: String },
    /// Our entries are added or updated.
    Update { diff: String },
    /// Already as it should be.
    Unchanged,
    /// Our entries are removed; everything else stays.
    Remove { diff: String },
    /// The file only held our entries and `wn setup` created it: it is deleted.
    Delete,
    /// Nothing of ours to remove.
    Absent,
    /// Not valid JSON (or not an object): left alone.
    Invalid,
    /// Left alone for another reason.
    Skipped { reason: String },
}

/// One agent's hook file in a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HookItem {
    pub agent: Agent,
    pub path: PathBuf,
    #[serde(flatten)]
    pub action: HookAction,
    /// The text to write (for create/update/remove).
    #[serde(skip)]
    pub write: Option<String>,
    /// The file as it was when planned (`None`: absent).
    #[serde(skip)]
    pub before: Option<String>,
}

/// Plans the hook file of `agent` at `path`. `record`: what an earlier `wn setup` recorded about
/// this file (whether it created it, and its bytes before the first install, which uninstall
/// restores exactly when nothing else changed since).
pub fn hook_item(
    agent: Agent,
    path: &Path,
    program: &str,
    uninstall: bool,
    record: Option<&HookRecord>,
) -> HookItem {
    let existing = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(_) => None,
    };
    let item = |action, write| HookItem {
        agent,
        path: path.to_path_buf(),
        action,
        write,
        before: existing.clone(),
    };
    if path.exists() && existing.is_none() {
        return item(HookAction::Invalid, None);
    }
    let value = match existing.as_deref() {
        None => Value::Object(Default::default()),
        Some(t) if t.trim().is_empty() => Value::Object(Default::default()),
        Some(t) => match serde_json::from_str::<Value>(t) {
            Ok(v) if v.is_object() => v,
            _ => return item(HookAction::Invalid, None),
        },
    };
    if !uninstall && agent == Agent::Codex && codex_inline_hooks(path) {
        return item(
            HookAction::Skipped {
                reason: "config.toml next to it defines hooks inline; add them there by hand (see docs/skill.md)".into(),
            },
            None,
        );
    }
    let mut after = value.clone();
    edit_hooks(&mut after, agent, program, !uninstall);
    if after == value {
        return item(
            if uninstall {
                HookAction::Absent
            } else {
                HookAction::Unchanged
            },
            None,
        );
    }
    let created = record.is_some_and(|r| r.created);
    if uninstall && created && is_bare(&after) {
        return item(HookAction::Delete, None);
    }
    // Edit the text in place when possible, so other bytes stay as they are and uninstall gives
    // back exactly what was there; otherwise re-serialise.
    let textual = existing
        .as_deref()
        .filter(|t| !t.trim().is_empty())
        .and_then(|t| remove_text(t, agent))
        .and_then(|t| {
            if uninstall {
                Some(t)
            } else {
                add_text(&t, agent, program)
            }
        })
        .filter(|t| serde_json::from_str::<Value>(t).is_ok_and(|v| v == after));
    let text = textual.unwrap_or_else(|| {
        let mut t = serde_json::to_string_pretty(&after).unwrap_or_default();
        t.push('\n');
        t
    });
    let d = diff(existing.as_deref().unwrap_or(""), &text);
    let action = match (&existing, uninstall) {
        (_, true) => HookAction::Remove { diff: d },
        (None, false) => HookAction::Create { diff: d },
        (Some(_), false) => HookAction::Update { diff: d },
    };
    item(action, Some(text))
}

/// A sync plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub version: String,
    pub items: Vec<(Target, Action)>,
    pub hooks: Vec<HookItem>,
}

impl Plan {
    /// Whether applying would change anything.
    pub fn has_changes(&self) -> bool {
        self.items
            .iter()
            .any(|(_, a)| matches!(a, Action::Create | Action::Update { .. } | Action::Remove))
            || self.hooks.iter().any(|h| {
                matches!(
                    h.action,
                    HookAction::Create { .. }
                        | HookAction::Update { .. }
                        | HookAction::Remove { .. }
                        | HookAction::Delete
                )
            })
    }
}

/// Unified line diff between two texts (only changed hunks, with 2 lines of context).
pub fn diff(old: &str, new: &str) -> String {
    similar::TextDiff::from_lines(old, new)
        .unified_diff()
        .context_radius(2)
        .header("installed", "new")
        .to_string()
}

/// Plans installing (or, with `uninstall`, removing) the skill at `targets` and the hooks in
/// `hook_files` (`(agent, file, what an earlier run recorded about it)`).
pub fn plan(
    targets: &[Target],
    uninstall: bool,
    hook_files: &[(Agent, PathBuf, Option<HookRecord>)],
    program: &str,
) -> Plan {
    let new = rendered();
    let items = targets
        .iter()
        .map(|t| {
            let existing = std::fs::read_to_string(&t.path).ok();
            let ours = existing.as_deref().is_some_and(|s| s.contains(MARKER));
            let action = match (uninstall, existing) {
                (true, None) => Action::Absent,
                (true, Some(_)) if ours => Action::Remove,
                (true, Some(_)) => Action::Absent,
                (false, None) => Action::Create,
                (false, Some(_)) if !ours => Action::Conflict,
                (false, Some(old)) if old == new => Action::Unchanged,
                (false, Some(old)) => Action::Update {
                    diff: diff(&old, &new),
                },
            };
            (t.clone(), action)
        })
        .collect();
    let hooks = hook_files
        .iter()
        .map(|(agent, path, record)| hook_item(*agent, path, program, uninstall, record.as_ref()))
        .collect();
    Plan {
        version: skill_version(),
        items,
        hooks,
    }
}

/// A hook file `wn setup` manages.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
pub struct HookRecord {
    pub agent: Agent,
    pub path: PathBuf,
    /// `wn setup` created the file (it is deleted again when only our entries remain).
    #[serde(default)]
    pub created: bool,
}

/// Recorded installs (`$WHERE_NEXT_HOME/skills.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: String,
    pub targets: Vec<Target>,
    /// Hook files (Claude settings, Codex and Cursor hooks.json).
    #[serde(default)]
    pub hooks: Vec<HookRecord>,
    /// Older releases recorded only a Claude settings file here.
    #[serde(default, skip_serializing)]
    pub hook_settings: Option<PathBuf>,
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join("skills.json")
}

pub fn load_state(home: &Path) -> State {
    let mut state: State = std::fs::read_to_string(state_path(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if let Some(path) = state.hook_settings.take() {
        if !state.hooks.iter().any(|h| h.path == path) {
            state.hooks.push(HookRecord {
                agent: Agent::Claude,
                path,
                created: false,
            });
        }
    }
    state
}

/// Replaces `path` with `text` atomically: through a symlink to its target, keeping the file's
/// permissions (`mode` for a new file), via a unique temporary file in the same directory.
fn write_file(path: &Path, text: &str, mode: u32) -> std::io::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
    let target = match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_symlink() => std::fs::canonicalize(path)?,
        _ => path.to_path_buf(),
    };
    let dir = target.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let keep = std::fs::metadata(&target).ok().map(|m| m.permissions());
    let name = target
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{name}.wn-{}-{nanos}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        let perms = keep.unwrap_or_else(|| std::fs::Permissions::from_mode(mode));
        std::fs::set_permissions(&tmp, perms)?;
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    write_file(path, text, 0o644)
}

/// Applies a plan and records the result. Returns human-readable lines.
pub fn apply(plan: &Plan, home: &Path, uninstall: bool) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut state = load_state(home);
    let new = rendered();
    // A settings file edited since the plan was shown is not overwritten.
    for h in &plan.hooks {
        let writes = matches!(
            h.action,
            HookAction::Create { .. }
                | HookAction::Update { .. }
                | HookAction::Remove { .. }
                | HookAction::Delete
        );
        if writes && std::fs::read_to_string(&h.path).ok() != h.before {
            return Err(format!(
                "{} changed since the plan was shown; nothing was written to it (run wn setup again)",
                h.path.display()
            ));
        }
    }
    for (target, action) in &plan.items {
        match action {
            Action::Create | Action::Update { .. } => {
                write_atomic(&target.path, &new)
                    .map_err(|e| format!("{}: {e}", target.path.display()))?;
                lines.push(format!("{}: wrote {}", target.agent, target.path.display()));
                if !state.targets.contains(target) {
                    state.targets.push(target.clone());
                }
            }
            Action::Remove => {
                std::fs::remove_file(&target.path)
                    .map_err(|e| format!("{}: {e}", target.path.display()))?;
                // `…/where-next`, then `skills` if that left it empty.
                for dir in target.path.ancestors().skip(1).take(2) {
                    if std::fs::remove_dir(dir).is_err() {
                        break;
                    }
                }
                lines.push(format!(
                    "{}: removed {}",
                    target.agent,
                    target.path.display()
                ));
                state.targets.retain(|t| t != target);
            }
            Action::Unchanged | Action::Conflict | Action::Absent => {
                if uninstall {
                    state.targets.retain(|t| t != target);
                } else if matches!(action, Action::Unchanged) && !state.targets.contains(target) {
                    state.targets.push(target.clone());
                }
            }
        }
    }
    for h in &plan.hooks {
        let recorded = state.hooks.iter().position(|r| r.path == h.path);
        match (&h.action, &h.write) {
            (HookAction::Create { .. } | HookAction::Update { .. }, Some(text)) => {
                write_atomic(&h.path, text).map_err(|e| format!("{}: {e}", h.path.display()))?;
                lines.push(format!("{} hooks: wrote {}", h.agent, h.path.display()));
                let previous = recorded.map(|i| state.hooks.remove(i));
                let created = previous.map_or(h.before.is_none(), |r| r.created);
                state.hooks.push(HookRecord {
                    agent: h.agent,
                    path: h.path.clone(),
                    created,
                });
            }
            (HookAction::Remove { .. }, Some(text)) => {
                write_atomic(&h.path, text).map_err(|e| format!("{}: {e}", h.path.display()))?;
                lines.push(format!(
                    "{} hooks: removed from {}",
                    h.agent,
                    h.path.display()
                ));
                state.hooks.retain(|r| r.path != h.path);
            }
            (HookAction::Delete, _) => {
                std::fs::remove_file(&h.path).map_err(|e| format!("{}: {e}", h.path.display()))?;
                lines.push(format!("{} hooks: deleted {}", h.agent, h.path.display()));
                state.hooks.retain(|r| r.path != h.path);
            }
            (HookAction::Unchanged, _) if recorded.is_none() => state.hooks.push(HookRecord {
                agent: h.agent,
                path: h.path.clone(),
                created: false,
            }),
            (HookAction::Absent, _) => state.hooks.retain(|r| r.path != h.path),
            _ => {}
        }
    }
    state.targets.sort();
    state.hooks.sort();
    state.version = plan.version.clone();
    let text = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    write_file(&state_path(home), &(text + "\n"), 0o600).map_err(|e| e.to_string())?;
    Ok(lines)
}

fn indent(out: &mut Vec<String>, text: &str) {
    for line in text.lines() {
        out.push(format!("         {line}"));
    }
}

/// Text form of a plan.
pub fn render_plan(plan: &Plan) -> String {
    let mut out = vec![format!("where-next skill {}", plan.version)];
    for (t, a) in &plan.items {
        if matches!(a, Action::Absent) {
            continue;
        }
        let what = match a {
            Action::Create => "create".to_string(),
            Action::Update { .. } => "update".to_string(),
            Action::Unchanged => "up to date".to_string(),
            Action::Conflict => "skip: a file not written by wn is there (left alone)".to_string(),
            Action::Remove => "remove".to_string(),
            Action::Absent => "nothing installed".to_string(),
        };
        out.push(format!(
            "  {:<6} {:<7} {}  ({what})",
            t.agent.to_string(),
            t.scope,
            t.path.display()
        ));
        if let Action::Update { diff } = a {
            indent(&mut out, diff);
        }
    }
    if plan
        .hooks
        .iter()
        .any(|h| !matches!(h.action, HookAction::Absent) || h.path.exists())
    {
        out.push("hooks (add wn's hints to the agent's context; WN_HOOKS=0 turns them off)".into());
    }
    for h in &plan.hooks {
        if matches!(h.action, HookAction::Absent) && !h.path.exists() {
            continue;
        }
        let what = match &h.action {
            HookAction::Create { .. } => "create".to_string(),
            HookAction::Update { .. } => "add where-next hooks".to_string(),
            HookAction::Unchanged => "up to date".to_string(),
            HookAction::Remove { .. } => "remove where-next hooks".to_string(),
            HookAction::Delete => "delete (only where-next hooks in it)".to_string(),
            HookAction::Absent => "no where-next hooks".to_string(),
            HookAction::Invalid => "not valid JSON; left alone".to_string(),
            HookAction::Skipped { reason } => format!("skip: {reason}"),
        };
        out.push(format!(
            "  {:<6} {}  ({what})",
            h.agent.to_string(),
            h.path.display()
        ));
        if let HookAction::Create { diff }
        | HookAction::Update { diff }
        | HookAction::Remove { diff } = &h.action
        {
            indent(&mut out, diff);
        }
    }
    out.join("\n")
}

/// States of one sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncState {
    /// Computing what would change.
    Planning,
    /// Showing the plan and waiting for a decision.
    Reviewing,
    /// Writing files.
    Applying,
    /// Terminal: applied, or nothing to do.
    Done,
    /// Terminal: plan shown, nothing written (`--dry-run`, or no `--yes` without a terminal).
    Previewed,
    /// Terminal: the user said no.
    Declined,
    /// Terminal: an error.
    Failed,
}

impl SyncState {
    pub const ALL: [SyncState; 7] = [
        SyncState::Planning,
        SyncState::Reviewing,
        SyncState::Applying,
        SyncState::Done,
        SyncState::Previewed,
        SyncState::Declined,
        SyncState::Failed,
    ];

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            SyncState::Done | SyncState::Previewed | SyncState::Declined | SyncState::Failed
        )
    }
}

/// Events of one sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncEvent {
    /// A plan with changes exists.
    Planned,
    /// The plan changes nothing.
    NothingToDo,
    /// Show only.
    Preview,
    /// Apply confirmed (`--yes` or "y").
    Confirm,
    /// Apply refused.
    Decline,
    /// Every write succeeded.
    Applied,
    /// Something failed.
    Error,
}

impl SyncEvent {
    pub const ALL: [SyncEvent; 7] = [
        SyncEvent::Planned,
        SyncEvent::NothingToDo,
        SyncEvent::Preview,
        SyncEvent::Confirm,
        SyncEvent::Decline,
        SyncEvent::Applied,
        SyncEvent::Error,
    ];
}

use SyncEvent as E;
use SyncState as S;

/// The complete transition table. Pairs not listed are illegal.
pub const TRANSITIONS: &[(SyncState, SyncEvent, SyncState)] = &[
    (S::Planning, E::Planned, S::Reviewing),
    (S::Planning, E::NothingToDo, S::Done),
    (S::Planning, E::Error, S::Failed),
    (S::Reviewing, E::Preview, S::Previewed),
    (S::Reviewing, E::Confirm, S::Applying),
    (S::Reviewing, E::Decline, S::Declined),
    (S::Applying, E::Applied, S::Done),
    (S::Applying, E::Error, S::Failed),
];

pub fn next(state: SyncState, event: SyncEvent) -> Option<SyncState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: SyncState,
    pub event: SyncEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in sync state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// One sync. The state only changes through [`SyncLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncLifecycle {
    state: SyncState,
}

impl Default for SyncLifecycle {
    fn default() -> Self {
        Self {
            state: SyncState::Planning,
        }
    }
}

impl SyncLifecycle {
    pub fn state(&self) -> SyncState {
        self.state
    }

    pub fn handle(&mut self, event: SyncEvent) -> Result<SyncState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}

fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Resolves the targets for a sync.
pub fn targets(
    agents: &[AgentArg],
    project: Option<&Path>,
    home: &Path,
    from_state: Option<&State>,
) -> Vec<Target> {
    if let Some(state) = from_state {
        return state.targets.clone();
    }
    let chosen: Vec<Agent> = if agents.contains(&AgentArg::All) {
        Agent::ALL.to_vec()
    } else if agents.is_empty() {
        let found: Vec<Agent> = Agent::ALL
            .into_iter()
            .filter(|a| a.detected(home))
            .collect();
        if found.is_empty() {
            vec![Agent::Claude]
        } else {
            found
        }
    } else {
        let mut v: Vec<Agent> = agents
            .iter()
            .filter_map(|a| match a {
                AgentArg::Claude => Some(Agent::Claude),
                AgentArg::Codex => Some(Agent::Codex),
                AgentArg::Cursor => Some(Agent::Cursor),
                AgentArg::All => None,
            })
            .collect();
        v.sort();
        v.dedup();
        v
    };
    let (base, scope) = match project {
        Some(root) => (root.to_path_buf(), "project"),
        None => (home.to_path_buf(), "user"),
    };
    chosen
        .into_iter()
        .map(|agent| Target {
            agent,
            scope: scope.to_string(),
            path: agent.skill_dir(&base).join("SKILL.md"),
        })
        .collect()
}

/// Report of `wn setup` (for `--json`).
#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    pub state: String,
    pub plan: Plan,
    pub applied: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Runs `wn skill …`.
pub fn run(action: &SkillAction, cli: &crate::Cli) -> (String, i32) {
    match action {
        SkillAction::Sync(args) => run_sync(args, cli),
        SkillAction::Show => (rendered(), 0),
    }
}

/// A hook file a run touches, with what an earlier run recorded about it.
pub type HookFile = (Agent, PathBuf, Option<HookRecord>);

fn hook_files(args: &SyncArgs, targets: &[Target], base: &Path, state: &State) -> Vec<HookFile> {
    let record = |p: &Path| state.hooks.iter().find(|r| r.path == p).cloned();
    if args.from_state {
        return state
            .hooks
            .iter()
            .map(|r| (r.agent, r.path.clone(), Some(r.clone())))
            .collect();
    }
    if args.no_hooks {
        return Vec::new();
    }
    let mut agents: Vec<Agent> = targets.iter().map(|t| t.agent).collect();
    agents.sort();
    agents.dedup();
    let mut out: Vec<HookFile> = agents
        .iter()
        .map(|a| {
            let p = a.hook_file(base);
            let r = record(&p);
            (*a, p, r)
        })
        .collect();
    if args.uninstall {
        // Also anything recorded for these agents elsewhere (e.g. `--project` installs).
        for r in &state.hooks {
            if agents.contains(&r.agent) && !out.iter().any(|(_, p, _)| *p == r.path) {
                out.push((r.agent, r.path.clone(), Some(r.clone())));
            }
        }
    }
    out
}

/// Everything a run touches: skill targets and hook files. `--uninstall` without `--agent` covers
/// every agent, plus every recorded install (any scope) for the chosen agents.
pub fn resolve(
    args: &SyncArgs,
    root: Option<&Path>,
    home: &Path,
    state: &State,
) -> (Vec<Target>, Vec<HookFile>) {
    let agents: Vec<AgentArg> = if args.uninstall && args.agents.is_empty() {
        vec![AgentArg::All]
    } else {
        args.agents.clone()
    };
    let recorded = args.from_state.then_some(state);
    let mut targets = targets(&agents, root, home, recorded);
    if args.uninstall && !args.from_state {
        let chosen: Vec<Agent> = targets.iter().map(|t| t.agent).collect();
        for t in &state.targets {
            if chosen.contains(&t.agent)
                && !targets.contains(t)
                && (root.is_none() || t.scope == "project")
            {
                targets.push(t.clone());
            }
        }
    }
    let base = root.unwrap_or(home);
    let hooks = hook_files(args, &targets, base, state);
    (targets, hooks)
}

fn after_notes(plan: &Plan, uninstall: bool) -> Vec<String> {
    let changed: Vec<&HookItem> = plan
        .hooks
        .iter()
        .filter(|h| {
            matches!(
                h.action,
                HookAction::Create { .. } | HookAction::Update { .. }
            )
        })
        .collect();
    if uninstall || changed.is_empty() {
        return Vec::new();
    }
    let mut notes = vec![
        "next:".to_string(),
        "  new agent sessions pick up the hooks; they answer only in indexed repositories (`wn init` once per repository)".to_string(),
    ];
    if changed.iter().any(|h| h.agent == Agent::Codex) {
        notes.push(
            "  Codex runs new hooks only after you trust them: open Codex, run /hooks, trust the where-next hooks".into(),
        );
    }
    notes.push(
        "  see what they did: wn stats · turn off: WN_HOOKS=0, or wn setup --uninstall".into(),
    );
    notes
}

/// Runs `wn setup` / `wn skill sync`.
pub fn run_sync(args: &SyncArgs, cli: &crate::Cli) -> (String, i32) {
    let wn_home = crate::home();
    let home = user_home();
    let root = args.project.then(|| wn_git::repo_root(&cli.path));
    let recorded = load_state(&wn_home);
    if args.from_state && recorded.targets.is_empty() && recorded.hooks.is_empty() {
        return ("no synced skills recorded; nothing to do".into(), 0);
    }
    let (targets, hooks) = resolve(args, root.as_deref(), &home, &recorded);
    let mut life = SyncLifecycle::default();
    let plan = plan(&targets, args.uninstall, &hooks, &hook_program());
    let mut applied = Vec::new();
    let mut message = None;
    if plan.has_changes() {
        let _ = life.handle(E::Planned);
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        let decision = if args.dry_run {
            E::Preview
        } else if args.yes {
            E::Confirm
        } else if interactive {
            eprintln!("{}", render_plan(&plan));
            eprint!("Apply these changes? [y/N] ");
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            if answer.trim().eq_ignore_ascii_case("y") || answer.trim().eq_ignore_ascii_case("yes")
            {
                E::Confirm
            } else {
                E::Decline
            }
        } else {
            message = Some("no terminal to confirm: rerun with --yes to apply".into());
            E::Preview
        };
        let _ = life.handle(decision);
        if life.state() == S::Applying {
            match apply(&plan, &wn_home, args.uninstall) {
                Ok(lines) => {
                    applied = lines;
                    let _ = life.handle(E::Applied);
                }
                Err(e) => {
                    message = Some(e);
                    let _ = life.handle(E::Error);
                }
            }
        }
    } else {
        let _ = life.handle(E::NothingToDo);
        if !args.dry_run {
            // Record up-to-date targets so `wn update` keeps them current.
            let _ = apply(&plan, &wn_home, args.uninstall);
        }
    }
    let report = SyncReport {
        state: format!("{:?}", life.state()).to_lowercase(),
        plan,
        applied,
        message,
    };
    let code = match life.state() {
        S::Failed => 1,
        S::Previewed if report.message.is_some() => 1,
        _ => 0,
    };
    if cli.json {
        return (
            serde_json::to_string_pretty(&report).unwrap_or_default(),
            code,
        );
    }
    let mut text = render_plan(&report.plan);
    for line in &report.applied {
        text.push('\n');
        text.push_str(line);
    }
    let tail = match life.state() {
        S::Done if report.applied.is_empty() => "nothing to change".to_string(),
        S::Done => "done".to_string(),
        S::Previewed => report
            .message
            .clone()
            .unwrap_or_else(|| "dry run: nothing written".into()),
        S::Declined => "not applied".to_string(),
        S::Failed => format!("failed: {}", report.message.clone().unwrap_or_default()),
        _ => String::new(),
    };
    text.push('\n');
    text.push_str(&tail);
    if life.state() == S::Done {
        for note in after_notes(&report.plan, args.uninstall) {
            text.push('\n');
            text.push_str(&note);
        }
    }
    (text, code)
}
