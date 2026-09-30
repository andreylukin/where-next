//! `wn uninstall`: removes everything wn put on this machine, after showing the full list and
//! asking once.
//!
//! In order: stop the daemon; remove the skill and hooks `wn setup` added to Claude Code, Codex
//! and Cursor (only entries it owns; see [`crate::skill`]); remove `$WHERE_NEXT_HOME` (indexes,
//! adapters, usage and hook logs, setup state), the models (unless `--keep-models`), the source
//! checkout of `wn update` (`$WN_HOME`), and finally the binary with the files the release
//! installer put beside it. Nothing outside those paths is touched; what it cannot remove (a
//! `PATH` line you added, a package manager's install) is listed as a note.
//!
//! The run goes through the same plan → review → apply machine as `wn setup`
//! ([`crate::skill::SyncLifecycle`]).

use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};

use clap::Args;
use serde::Serialize;

use crate::skill::{self, SyncArgs, SyncEvent as E, SyncLifecycle, SyncState as S};

/// Options of `wn uninstall`.
#[derive(Debug, Clone, Args)]
pub struct UninstallArgs {
    /// Remove without asking.
    #[arg(long, short)]
    pub yes: bool,
    /// Keep the downloaded models (~1.2 GB for the default one).
    #[arg(long)]
    pub keep_models: bool,
    /// Show what would be removed; remove nothing.
    #[arg(long)]
    pub dry_run: bool,
}

pub const UNINSTALL_EXAMPLES: &str = "\
Removes the where-next skill and hooks from your agents, the daemon, indexes, logs, models, the
source checkout and the wn binary. Shows the full list first and asks once.

Examples:
  wn uninstall --dry-run                   show what would be removed
  wn uninstall                             ask, then remove everything
  wn uninstall --keep-models --yes         keep the downloaded models";

/// One path to remove.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removal {
    pub what: String,
    pub path: PathBuf,
}

/// What `wn uninstall` will do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    /// A daemon answers on the socket and will be stopped.
    pub daemon: bool,
    /// Skill files and hook entries in agent settings.
    pub agents: skill::Plan,
    /// Files and directories removed, in order (the binary last).
    pub paths: Vec<Removal>,
    /// Kept on purpose (`--keep-models`).
    pub kept: Vec<Removal>,
    /// Things to do by hand.
    pub notes: Vec<String>,
}

/// Where things live (explicit for tests).
#[derive(Debug, Clone)]
pub struct Locations {
    pub exe: Option<PathBuf>,
    pub wn_home: PathBuf,
    pub models: PathBuf,
    pub data: PathBuf,
    pub user_home: PathBuf,
}

impl Locations {
    pub fn detect() -> Locations {
        Locations {
            exe: std::env::current_exe().ok(),
            wn_home: crate::home(),
            models: crate::models_home(),
            data: crate::update::wn_home(),
            user_home: std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_else(std::env::temp_dir),
        }
    }
}

/// Plans the uninstall.
pub fn plan(loc: &Locations, keep_models: bool) -> Plan {
    let setup = uninstall_args();
    let state = skill::load_state(&loc.wn_home);
    let (targets, hooks) = skill::resolve(&setup, None, &loc.user_home, &state);
    let agents = skill::plan(&targets, true, &hooks, "wn");
    let mut paths = Vec::new();
    let mut kept = Vec::new();
    let mut notes = Vec::new();
    let mut add = |what: &str, path: &Path| {
        if path.exists() {
            paths.push(Removal {
                what: what.into(),
                path: path.to_path_buf(),
            });
        }
    };
    add(
        "indexes, adapters, usage and hook logs, setup state",
        &loc.wn_home,
    );
    let socket = crate::daemon::socket_path(&loc.wn_home);
    if !socket.starts_with(&loc.wn_home) {
        add("daemon socket", &socket);
    }
    if keep_models {
        if loc.models.exists() {
            kept.push(Removal {
                what: "models (--keep-models)".into(),
                path: loc.models.clone(),
            });
        }
    } else {
        add("models", &loc.models);
    }
    add("source checkout used by `wn update`", &loc.data);
    if let Some(exe) = &loc.exe {
        let text = exe.to_string_lossy();
        let managed = if text.contains("/Cellar/") || text.contains("/homebrew/") {
            Some("brew uninstall where-next")
        } else if text.contains("/node_modules/") {
            Some("npm uninstall -g <the package that installed it>")
        } else {
            None
        };
        match managed {
            Some(cmd) => notes.push(format!(
                "the wn binary belongs to a package manager: remove it with `{cmd}`"
            )),
            None => {
                if let Some(dir) = exe.parent() {
                    add(
                        "ONNX Runtime library installed with wn",
                        &dir.join("libonnxruntime.so"),
                    );
                    add("installer marker", &dir.join("wn.install-method"));
                    if dir.ends_with(".cargo/bin") {
                        notes.push(
                            "cargo still lists the package: `cargo uninstall where-next` clears that record"
                                .into(),
                        );
                    }
                    notes.push(format!(
                        "if you added {} to PATH in a shell profile, remove that line yourself",
                        dir.display()
                    ));
                }
                add("the wn binary", exe);
            }
        }
    }
    Plan {
        daemon: crate::daemon::client::stats(&loc.wn_home).is_some(),
        agents,
        paths,
        kept,
        notes,
    }
}

fn uninstall_args() -> SyncArgs {
    SyncArgs {
        agents: Vec::new(),
        project: false,
        dry_run: false,
        uninstall: true,
        yes: true,
        no_hooks: false,
        from_state: false,
        with_hook: false,
    }
}

impl Plan {
    fn is_empty(&self) -> bool {
        !self.daemon && !self.agents.has_changes() && self.paths.is_empty()
    }
}

/// Text form of a plan.
pub fn render(p: &Plan) -> String {
    let mut out = vec!["wn uninstall will remove:".to_string()];
    if p.daemon {
        out.push("  the running wn daemon (stopped first)".into());
    }
    if p.agents.has_changes() {
        for (t, a) in &p.agents.items {
            if matches!(a, skill::Action::Remove) {
                out.push(format!(
                    "  {} skill        {}",
                    t.agent.label(),
                    t.path.display()
                ));
            }
        }
        for h in &p.agents.hooks {
            match h.action {
                skill::HookAction::Remove { .. } => out.push(format!(
                    "  {} hooks        where-next entries in {} (your other settings stay)",
                    h.agent.label(),
                    h.path.display()
                )),
                skill::HookAction::Delete => out.push(format!(
                    "  {} hooks        {} (created by wn setup; only where-next hooks in it)",
                    h.agent.label(),
                    h.path.display()
                )),
                _ => {}
            }
        }
    }
    for r in &p.paths {
        out.push(format!("  {}  ({})", r.path.display(), r.what));
    }
    if p.is_empty() {
        out.push("  nothing: no wn files found".into());
    }
    for r in &p.kept {
        out.push(format!("kept: {}  ({})", r.path.display(), r.what));
    }
    out.join("\n")
}

fn remove(path: &Path) -> std::io::Result<()> {
    let meta = std::fs::symlink_metadata(path)?;
    if meta.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    }
}

/// Applies a plan: returns what was removed and what could not be.
pub fn apply(p: &Plan, loc: &Locations) -> (Vec<String>, Vec<String>) {
    let mut done = Vec::new();
    let mut failed = Vec::new();
    if p.daemon {
        if crate::daemon::client::stop(&loc.wn_home) {
            done.push("stopped the daemon".to_string());
        } else {
            failed.push("the daemon did not stop (`wn daemon stop`)".to_string());
        }
    }
    if p.agents.has_changes() {
        match skill::apply(&p.agents, &loc.wn_home, true) {
            Ok(lines) => done.extend(lines),
            Err(e) => failed.push(format!("agent settings: {e}")),
        }
    }
    for r in &p.paths {
        match remove(&r.path) {
            Ok(()) => done.push(format!("removed {}", r.path.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => failed.push(format!("{}: {e}", r.path.display())),
        }
    }
    (done, failed)
}

/// Report of `wn uninstall` (for `--json`).
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub state: String,
    pub plan: Plan,
    pub removed: Vec<String>,
    pub failed: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// Runs `wn uninstall`.
pub fn run(args: &UninstallArgs, json: bool) -> (String, i32) {
    let loc = Locations::detect();
    let plan = plan(&loc, args.keep_models);
    let mut life = SyncLifecycle::default();
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    let mut message = None;
    if plan.is_empty() {
        let _ = life.handle(E::NothingToDo);
    } else {
        let _ = life.handle(E::Planned);
        let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
        let decision = if args.dry_run {
            E::Preview
        } else if args.yes {
            E::Confirm
        } else if interactive {
            eprintln!("{}", render(&plan));
            eprint!("Remove all of this? [y/N] ");
            let mut answer = String::new();
            let _ = std::io::stdin().read_line(&mut answer);
            if matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
                E::Confirm
            } else {
                E::Decline
            }
        } else {
            message = Some("no terminal to confirm: rerun with --yes to remove".to_string());
            E::Preview
        };
        let _ = life.handle(decision);
        if life.state() == S::Applying {
            (removed, failed) = apply(&plan, &loc);
            let _ = life.handle(if failed.is_empty() {
                E::Applied
            } else {
                E::Error
            });
        }
    }
    let code = match life.state() {
        S::Failed => 1,
        S::Previewed if message.is_some() => 1,
        _ => 0,
    };
    let report = Report {
        state: format!("{:?}", life.state()).to_lowercase(),
        plan,
        removed,
        failed,
        message,
    };
    if json {
        return (
            serde_json::to_string_pretty(&report).unwrap_or_default(),
            code,
        );
    }
    let mut out = Vec::new();
    match life.state() {
        S::Done | S::Failed if !report.removed.is_empty() || !report.failed.is_empty() => {
            out.push("removed:".to_string());
            out.extend(report.removed.iter().map(|l| format!("  {l}")));
            if !report.failed.is_empty() {
                out.push("could not remove:".to_string());
                out.extend(report.failed.iter().map(|l| format!("  {l}")));
            }
        }
        S::Done => out.push("nothing to remove: no wn files found".into()),
        _ => {
            out.push(render(&report.plan));
            out.push(match life.state() {
                S::Declined => "not removed".to_string(),
                _ => report
                    .message
                    .clone()
                    .unwrap_or_else(|| "dry run: nothing removed".into()),
            });
        }
    }
    for r in &report.plan.kept {
        out.push(format!("kept: {}  ({})", r.path.display(), r.what));
    }
    for n in &report.plan.notes {
        out.push(format!("note: {n}"));
    }
    (out.join("\n"), code)
}
