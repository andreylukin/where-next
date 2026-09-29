//! `wn skill sync`: installs the where-next agent skill (`SKILL.md`) for Claude Code, Codex and
//! Cursor, keeps it current, and optionally adds a Claude Code hook that injects task-start hints.
//!
//! All three agents read the same `SKILL.md` format from their own directories:
//!
//! | agent  | user scope                            | project scope (`--project`)     |
//! |--------|---------------------------------------|---------------------------------|
//! | claude | `~/.claude/skills/where-next/`         | `<repo>/.claude/skills/where-next/` |
//! | codex  | `~/.agents/skills/where-next/`         | `<repo>/.agents/skills/where-next/` |
//! | cursor | `~/.cursor/skills/where-next/`         | `<repo>/.cursor/skills/where-next/` |
//!
//! Only files carrying the managed marker are updated or removed; anything else at the target is
//! reported as a conflict and left alone. Installed targets are recorded in
//! `$WHERE_NEXT_HOME/skills.json` so `wn update` can re-sync them (`--from-state`).
//!
//! The sync itself is an explicit state machine ([`SyncState`]): plan, review (diff + confirm),
//! apply.

use std::fmt;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};

/// The skill source, compiled in so `wn skill sync` works without a checkout.
pub const SKILL: &str = include_str!("../assets/where-next/SKILL.md");

/// Marker line identifying files this command manages.
pub const MARKER: &str = "<!-- managed by `wn skill sync`";

/// Command a Claude Code hook runs (also how our hook entry is recognised).
pub const HOOK_COMMAND: &str = "wn hook claude-prompt";

/// `wn skill …`
#[derive(Debug, Clone, Subcommand)]
pub enum SkillAction {
    /// Install or update the where-next skill for coding agents.
    #[command(after_help = SYNC_EXAMPLES)]
    Sync {
        /// Agents to sync (default: those detected in your home directory).
        #[arg(long = "agent", value_enum)]
        agents: Vec<AgentArg>,
        /// Install into this repository (`.claude/skills`, `.agents/skills`, `.cursor/skills`)
        /// instead of your home directory.
        #[arg(long)]
        project: bool,
        /// Show what would change without writing anything.
        #[arg(long)]
        dry_run: bool,
        /// Remove the skill (and hook) instead.
        #[arg(long)]
        uninstall: bool,
        /// Apply without asking.
        #[arg(long, short)]
        yes: bool,
        /// Re-sync exactly the targets recorded by earlier syncs (used by `wn update`).
        #[arg(long)]
        from_state: bool,
        /// Also add a Claude Code hook that runs `wn ask --start` on the first prompt of a session
        /// in large repositories and adds the hints to the context (off by default).
        #[arg(long)]
        with_hook: bool,
    },
    /// Print the skill.
    Show,
}

const SYNC_EXAMPLES: &str = "\
Shows what it will write and asks first. Only a marked where-next block is ever edited, and
`wn update` re-syncs installed skills.

Examples:
  wn skill sync --dry-run                  show what would change, write nothing
  wn skill sync                            agents detected in your home directory
  wn skill sync --agent claude --yes
  wn skill sync --project                  this repository's .claude/.agents/.cursor skills
  wn skill sync --with-hook                also the Claude Code start-hint hook (large repos only)
  wn skill sync --uninstall";

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

    /// Whether the agent looks installed for the user whose home is `home`.
    pub fn detected(self, home: &Path) -> bool {
        match self {
            Agent::Claude => home.join(".claude").is_dir(),
            Agent::Codex => home.join(".codex").is_dir() || home.join(".agents").is_dir(),
            Agent::Cursor => home.join(".cursor").is_dir(),
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

/// What happens to the Claude Code hook, when requested.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum HookAction {
    Add {
        settings: PathBuf,
    },
    Present {
        settings: PathBuf,
    },
    Remove {
        settings: PathBuf,
    },
    Absent {
        settings: PathBuf,
    },
    /// The settings file exists but is not valid JSON; left alone.
    Invalid {
        settings: PathBuf,
    },
}

/// A sync plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub version: String,
    pub items: Vec<(Target, Action)>,
    pub hook: Option<HookAction>,
}

impl Plan {
    /// Whether applying would change anything.
    pub fn has_changes(&self) -> bool {
        self.items
            .iter()
            .any(|(_, a)| matches!(a, Action::Create | Action::Update { .. } | Action::Remove))
            || matches!(
                self.hook,
                Some(HookAction::Add { .. } | HookAction::Remove { .. })
            )
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

/// Plans installing (or, with `uninstall`, removing) the skill at `targets`.
pub fn plan(targets: &[Target], uninstall: bool, hook: Option<(PathBuf, bool)>) -> Plan {
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
    let hook = hook.map(|(settings, remove)| hook_plan(&settings, remove));
    Plan {
        version: skill_version(),
        items,
        hook,
    }
}

fn has_our_hook(settings: &serde_json::Value) -> bool {
    settings
        .pointer("/hooks/UserPromptSubmit")
        .and_then(|v| v.as_array())
        .is_some_and(|entries| {
            entries.iter().any(|e| {
                e.get("hooks").and_then(|h| h.as_array()).is_some_and(|hs| {
                    hs.iter()
                        .any(|h| h.get("command").and_then(|c| c.as_str()) == Some(HOOK_COMMAND))
                })
            })
        })
}

fn hook_plan(settings: &Path, remove: bool) -> HookAction {
    let path = settings.to_path_buf();
    let value = match std::fs::read_to_string(settings) {
        Err(_) => serde_json::json!({}),
        Ok(text) if text.trim().is_empty() => serde_json::json!({}),
        Ok(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) if v.is_object() => v,
            _ => return HookAction::Invalid { settings: path },
        },
    };
    match (remove, has_our_hook(&value)) {
        (false, true) => HookAction::Present { settings: path },
        (false, false) => HookAction::Add { settings: path },
        (true, true) => HookAction::Remove { settings: path },
        (true, false) => HookAction::Absent { settings: path },
    }
}

/// Adds or removes our hook entry in a Claude Code settings value; other content is untouched.
pub fn edit_hook(settings: &mut serde_json::Value, add: bool) {
    let Some(obj) = settings.as_object_mut() else {
        return;
    };
    let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
    let Some(hooks) = hooks.as_object_mut() else {
        return;
    };
    let list = hooks
        .entry("UserPromptSubmit")
        .or_insert_with(|| serde_json::json!([]));
    let Some(list) = list.as_array_mut() else {
        return;
    };
    // Drop our command from every entry, then drop entries left empty.
    for entry in list.iter_mut() {
        if let Some(hs) = entry.get_mut("hooks").and_then(|h| h.as_array_mut()) {
            hs.retain(|h| h.get("command").and_then(|c| c.as_str()) != Some(HOOK_COMMAND));
        }
    }
    list.retain(|e| {
        e.get("hooks")
            .and_then(|h| h.as_array())
            .map_or(true, |hs| !hs.is_empty())
    });
    if add {
        list.push(serde_json::json!({
            "hooks": [{ "type": "command", "command": HOOK_COMMAND, "timeout": 15 }]
        }));
    }
    let empty_list = list.is_empty();
    if empty_list {
        hooks.remove("UserPromptSubmit");
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
}

/// Recorded installs (`$WHERE_NEXT_HOME/skills.json`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    pub version: String,
    pub targets: Vec<Target>,
    #[serde(default)]
    pub hook_settings: Option<PathBuf>,
}

pub fn state_path(home: &Path) -> PathBuf {
    home.join("skills.json")
}

pub fn load_state(home: &Path) -> State {
    std::fs::read_to_string(state_path(home))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = path.with_extension("wn-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

/// Applies a plan and records the result. Returns human-readable lines.
pub fn apply(plan: &Plan, home: &Path, uninstall: bool) -> Result<Vec<String>, String> {
    let mut lines = Vec::new();
    let mut state = load_state(home);
    let new = rendered();
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
                if let Some(dir) = target.path.parent() {
                    let _ = std::fs::remove_dir(dir);
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
    if let Some(hook) = &plan.hook {
        match hook {
            HookAction::Add { settings } | HookAction::Remove { settings } => {
                let add = matches!(hook, HookAction::Add { .. });
                let mut value = std::fs::read_to_string(settings)
                    .ok()
                    .filter(|t| !t.trim().is_empty())
                    .and_then(|t| serde_json::from_str(&t).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                edit_hook(&mut value, add);
                let mut text = serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?;
                text.push('\n');
                write_atomic(settings, &text)
                    .map_err(|e| format!("{}: {e}", settings.display()))?;
                lines.push(format!(
                    "claude hook: {} {}",
                    if add { "added to" } else { "removed from" },
                    settings.display()
                ));
                state.hook_settings = add.then(|| settings.clone());
            }
            HookAction::Present { settings } => state.hook_settings = Some(settings.clone()),
            HookAction::Absent { .. } | HookAction::Invalid { .. } => {}
        }
    }
    state.targets.sort();
    state.version = plan.version.clone();
    let text = serde_json::to_string_pretty(&state).map_err(|e| e.to_string())?;
    write_atomic(&state_path(home), &(text + "\n")).map_err(|e| e.to_string())?;
    Ok(lines)
}

/// Text form of a plan.
pub fn render_plan(plan: &Plan) -> String {
    let mut out = vec![format!("where-next skill {}", plan.version)];
    for (t, a) in &plan.items {
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
            for line in diff.lines() {
                out.push(format!("         {line}"));
            }
        }
    }
    if let Some(h) = &plan.hook {
        out.push(match h {
            HookAction::Add { settings } => format!("  claude hook: add to {}", settings.display()),
            HookAction::Present { settings } => {
                format!("  claude hook: already in {}", settings.display())
            }
            HookAction::Remove { settings } => {
                format!("  claude hook: remove from {}", settings.display())
            }
            HookAction::Absent { settings } => {
                format!("  claude hook: not in {}", settings.display())
            }
            HookAction::Invalid { settings } => {
                format!(
                    "  claude hook: {} is not valid JSON; left alone",
                    settings.display()
                )
            }
        });
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

/// Report of `wn skill sync` (for `--json`).
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
    let SkillAction::Sync {
        agents,
        project,
        dry_run,
        uninstall,
        yes,
        from_state,
        with_hook,
    } = action
    else {
        return (rendered(), 0);
    };
    let wn_home = crate::home();
    let home = user_home();
    let root = project.then(|| wn_git::repo_root(&cli.path));
    let state = from_state.then(|| load_state(&wn_home));
    if state
        .as_ref()
        .is_some_and(|s| s.targets.is_empty() && s.hook_settings.is_none())
    {
        return ("no synced skills recorded; nothing to do".into(), 0);
    }
    let targets = targets(agents, root.as_deref(), &home, state.as_ref());
    let hook_settings = if *from_state {
        state.as_ref().and_then(|s| s.hook_settings.clone())
    } else if *with_hook || (*uninstall && load_state(&wn_home).hook_settings.is_some()) {
        Some(match &root {
            Some(r) => r.join(".claude").join("settings.json"),
            None => home.join(".claude").join("settings.json"),
        })
    } else {
        None
    };
    let hook = hook_settings.map(|s| (s, *uninstall));
    let mut life = SyncLifecycle::default();
    let plan = plan(&targets, *uninstall, hook);
    let mut applied = Vec::new();
    let mut message = None;
    if plan.has_changes() {
        let _ = life.handle(E::Planned);
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        let decision = if *dry_run {
            E::Preview
        } else if *yes {
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
            match apply(&plan, &wn_home, *uninstall) {
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
        if !*dry_run {
            // Record up-to-date targets so `wn update` keeps them current.
            let _ = apply(&plan, &wn_home, *uninstall);
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
    (text, code)
}

/// `wn hook claude-prompt`: reads a Claude Code `UserPromptSubmit` event from stdin and, on the
/// first prompt of a session in a large repository, prints where-next hints for Claude's context.
/// Prints nothing otherwise; always exits 0 so a hook never blocks a prompt.
pub fn claude_prompt_hook(input: &str) -> String {
    #[derive(Deserialize)]
    struct Event {
        #[serde(default)]
        session_id: String,
        #[serde(default)]
        prompt: String,
        #[serde(default)]
        cwd: Option<String>,
    }
    let Ok(event) = serde_json::from_str::<Event>(input) else {
        return String::new();
    };
    if event.prompt.trim().is_empty() || !first_prompt_of(&event.session_id) {
        return String::new();
    }
    let cwd = event.cwd.unwrap_or_else(|| ".".into());
    let parsed = <crate::Cli as clap::Parser>::try_parse_from([
        "wn",
        "--path",
        cwd.as_str(),
        "--json",
        "ask",
        "--start",
        event.prompt.as_str(),
    ]);
    let Ok(cli) = parsed else {
        return String::new();
    };
    let (json, _) = crate::run(cli);
    let Ok(outcome) = serde_json::from_str::<wn_core::rank::Outcome>(&json) else {
        return String::new();
    };
    if outcome.state != wn_core::rank::AnswerState::Ok || outcome.hints.files.is_empty() {
        return String::new();
    }
    let mut lines = vec![
        "where-next (local index of this repository) suggests starting with these files; verify before relying on them:".to_string(),
    ];
    for h in &outcome.hints.files {
        lines.push(format!("- {} (similarity {:.2})", h.path, h.similarity));
    }
    lines.join("\n")
}

/// Records the session and returns whether this is its first prompt.
fn first_prompt_of(session: &str) -> bool {
    if session.is_empty() {
        return true;
    }
    let path = crate::home().join("hook-sessions.json");
    let mut seen: Vec<String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    if seen.iter().any(|s| s == session) {
        return false;
    }
    seen.push(session.to_string());
    let keep = seen.len().saturating_sub(200);
    let _ = write_atomic(
        &path,
        &serde_json::to_string(&seen[keep..]).unwrap_or_else(|_| "[]".into()),
    );
    true
}
