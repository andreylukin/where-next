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

/// What to remove under one location: exactly `items` (files or directories wn created), then
/// `path` itself when it is a directory left empty. Nothing else in `path` is touched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removal {
    pub what: String,
    pub path: PathBuf,
    pub items: Vec<PathBuf>,
    /// Entries in `path` that are not wn's and stay.
    pub others: usize,
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

/// Which wn directory a root is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `$WHERE_NEXT_HOME`.
    Cache,
    /// `$WN_MODELS_HOME`.
    Models,
    /// `$WN_HOME` (source checkout).
    Data,
}

impl Kind {
    fn var(self) -> &'static str {
        match self {
            Kind::Cache => "WHERE_NEXT_HOME",
            Kind::Models => "WN_MODELS_HOME",
            Kind::Data => "WN_HOME",
        }
    }
}

fn canon(p: &Path) -> PathBuf {
    p.canonicalize().unwrap_or_else(|_| p.to_path_buf())
}

/// Refuses a directory that must never be removed recursively: empty or relative paths, `/`, a
/// top-level directory, your home directory or anything containing it.
pub fn check_root(kind: Kind, path: &Path, home: &Path) -> Result<(), String> {
    let refuse = |why: &str| {
        Err(format!(
            "refusing to remove {} ({}): {why}",
            if path.as_os_str().is_empty() {
                "\"\"".to_string()
            } else {
                path.display().to_string()
            },
            kind.var()
        ))
    };
    if path.as_os_str().is_empty() {
        return refuse("the path is empty");
    }
    if !path.is_absolute() {
        return refuse("the path is relative");
    }
    let p = canon(path);
    match p.parent() {
        None => return refuse("it is /"),
        Some(parent) if parent.parent().is_none() => return refuse("it is a top-level directory"),
        _ => {}
    }
    let h = canon(home);
    if p == h {
        return refuse("it is your home directory");
    }
    if h.starts_with(&p) {
        return refuse("it contains your home directory");
    }
    Ok(())
}

fn repo_dir_name(name: &str) -> bool {
    name.rsplit_once('-').is_some_and(|(stem, hash)| {
        !stem.is_empty() && hash.len() == 16 && hash.bytes().all(|b| b.is_ascii_hexdigit())
    })
}

/// The entries of `root` that wn created, and how many others there are.
pub fn owned(kind: Kind, root: &Path) -> (Vec<PathBuf>, usize) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return (Vec::new(), 0);
    };
    let mut ours = Vec::new();
    let mut others = 0;
    for e in entries.flatten() {
        let path = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        let dir = path.is_dir() && !e.file_type().is_ok_and(|t| t.is_symlink());
        let has = |f: &str| path.join(f).exists();
        let mine = match kind {
            Kind::Cache => {
                matches!(
                    name.as_str(),
                    "daemon.log"
                        | "daemon.sock"
                        | "skills.json"
                        | "hook-log.jsonl"
                        | "fingerprints.json"
                ) || (dir && name == "hook-sessions")
                    || name.starts_with(".skills.json.wn-")
                    || (dir && repo_dir_name(&name))
                    || (dir
                        && name == "models"
                        && std::fs::read_dir(&path).is_ok_and(|mut it| {
                            it.any(|m| {
                                m.ok().is_some_and(|m| {
                                    let p = m.path();
                                    p.join("wn-model.json").exists()
                                        || p.join("model.safetensors").exists()
                                })
                            })
                        }))
            }
            Kind::Models => {
                dir && (has("wn-model.json")
                    || (name.starts_with('.') && name.ends_with(".pulling")))
            }
            Kind::Data => dir && name == "src" && has(".git"),
        };
        if mine {
            ours.push(path);
        } else {
            others += 1;
        }
    }
    ours.sort();
    (ours, others)
}

/// Files the release installer put beside the binary, when its records say so: the paths listed
/// in `wn.install-files` (which must list this binary), or, for installs from before that file,
/// a `wn.install-method` that says `release` plus the bundled `libonnxruntime.so`.
fn installer_files(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return Vec::new();
    };
    let marker = dir.join("wn.install-method");
    let manifest = dir.join("wn.install-files");
    let sidecar = |p: &Path| {
        p.parent() == Some(dir)
            && matches!(
                p.file_name().and_then(|n| n.to_str()),
                Some("libonnxruntime.so" | "wn.install-method" | "wn.install-files")
            )
    };
    if let Ok(text) = std::fs::read_to_string(&manifest) {
        let listed: Vec<PathBuf> = text.lines().map(PathBuf::from).collect();
        if !listed.iter().any(|p| canon(p) == canon(exe)) {
            return Vec::new();
        }
        let mut out: Vec<PathBuf> = listed
            .into_iter()
            .filter(|p| sidecar(p) && p.exists())
            .collect();
        out.push(manifest);
        if marker.exists() && !out.contains(&marker) {
            out.push(marker);
        }
        return out;
    }
    if std::fs::read_to_string(&marker).is_ok_and(|t| t.trim() == "release") {
        let lib = dir.join("libonnxruntime.so");
        return [lib, marker].into_iter().filter(|p| p.exists()).collect();
    }
    Vec::new()
}

/// Plans the uninstall. Refuses (before looking at anything else) a location it must not remove.
pub fn plan(loc: &Locations, keep_models: bool) -> Result<Plan, String> {
    for (kind, path) in [
        (Kind::Cache, &loc.wn_home),
        (Kind::Models, &loc.models),
        (Kind::Data, &loc.data),
    ] {
        check_root(kind, path, &loc.user_home)?;
    }
    let mut paths = Vec::new();
    let mut kept = Vec::new();
    let mut notes = Vec::new();
    let root = |kind: Kind, what: &str, path: &Path| -> Result<Option<Removal>, String> {
        if !path.exists() {
            return Ok(None);
        }
        let (items, others) = owned(kind, path);
        if items.is_empty() && others > 0 {
            return Err(format!(
                "refusing to touch {} ({}): nothing in it looks like wn's",
                path.display(),
                kind.var()
            ));
        }
        Ok(Some(Removal {
            what: what.into(),
            path: path.to_path_buf(),
            items,
            others,
        }))
    };
    let cache = root(
        Kind::Cache,
        "indexes, adapters, usage and hook logs, setup state",
        &loc.wn_home,
    )?;
    let models = root(Kind::Models, "models", &loc.models)?;
    let data = root(Kind::Data, "source checkout used by `wn update`", &loc.data)?;
    paths.extend(cache);
    let single = |what: &str, path: &Path| Removal {
        what: what.into(),
        path: path.to_path_buf(),
        items: vec![path.to_path_buf()],
        others: 0,
    };
    let socket = crate::daemon::socket_path(&loc.wn_home);
    if !socket.starts_with(&loc.wn_home) && socket.exists() {
        paths.push(single("daemon socket", &socket));
    }
    match (models, keep_models) {
        (Some(m), true) => kept.push(Removal {
            what: "models (--keep-models)".into(),
            ..m
        }),
        (Some(m), false) => paths.push(m),
        (None, _) => {}
    }
    paths.extend(data);
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
                for f in installer_files(exe) {
                    paths.push(single("installed with wn by install.sh", &f));
                }
                if let Some(dir) = exe.parent() {
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
                paths.push(single("the wn binary", exe));
            }
        }
    }
    for r in paths.iter().filter(|r| r.others > 0) {
        notes.push(format!(
            "{} other entries in {} are not wn's and stay",
            r.others,
            r.path.display()
        ));
    }
    let setup = uninstall_args();
    let state = skill::load_state(&loc.wn_home);
    let (targets, hooks) = skill::resolve(&setup, None, &loc.user_home, &state);
    let agents = skill::plan(&targets, true, &hooks, "wn");
    Ok(Plan {
        daemon: crate::daemon::client::stats(&loc.wn_home).is_some(),
        agents,
        paths,
        kept,
        notes,
    })
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
        if r.items == [r.path.clone()] {
            out.push(format!("  {}  ({})", r.path.display(), r.what));
        } else {
            out.push(format!(
                "  {}  ({}: {} {} wn created{})",
                r.path.display(),
                r.what,
                r.items.len(),
                if r.items.len() == 1 {
                    "entry"
                } else {
                    "entries"
                },
                if r.others > 0 {
                    "; other files stay"
                } else {
                    ""
                }
            ));
        }
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
        let mut ok = true;
        for item in &r.items {
            match remove(item) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    ok = false;
                    failed.push(format!("{}: {e}", item.display()));
                }
            }
        }
        // The directory itself only when nothing else is left in it.
        if r.items != [r.path.clone()] && r.path.is_dir() {
            let _ = std::fs::remove_dir(&r.path);
        }
        if r.what == "daemon socket" && ok {
            if let Some(dir) = r.path.parent() {
                let _ = std::fs::remove_dir(dir);
            }
        }
        if ok {
            done.push(format!("removed {}", r.path.display()));
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
    let plan = match plan(&loc, args.keep_models) {
        Ok(p) => p,
        Err(e) => return (format!("wn uninstall: {e}; nothing was removed"), 1),
    };
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
