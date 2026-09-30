//! `wn update`: re-install a release binary, or rebuild a source installation from its repository.
//!
//! The installer (`install.sh`) and this command share one layout: a private clone at
//! `$WN_HOME/src` (default `~/.local/share/where-next/src`), built with
//! `cargo install --path crates/wn-cli --locked`. The flow is an explicit state machine
//! ([`UpdateLifecycle`]) so every step and failure has a named state:
//!
//! `Idle --Start--> Fetching --FetchedSame--> UpToDate` (or `--FetchedNewer--> UpdateAvailable
//! --Build--> Building --BuildSucceeded--> Installed`); fetch and build failures go to `Failed`,
//! which can `Retry`. `UpToDate --Build--> Building` is `--force`.

use std::fmt;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::Serialize;

/// The commit this binary was built from (empty when not built from a git checkout).
pub const BUILD_COMMIT: &str = env!("WN_GIT_COMMIT");

/// Default source repository.
pub const DEFAULT_REPO: &str = "https://github.com/andreylukin/where-next";

/// Exit code of `wn update --check` when an update is available.
pub const EXIT_UPDATE_AVAILABLE: i32 = 10;

/// States of an update run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub enum UpdateState {
    /// Nothing done yet.
    Idle,
    /// Cloning or fetching the source repository.
    Fetching,
    /// The installed binary already matches the requested ref.
    UpToDate,
    /// The requested ref differs from the installed binary.
    UpdateAvailable,
    /// The release installer or `cargo install` is running.
    Building,
    /// The new binary is installed.
    Installed,
    /// A fetch or build failed.
    Failed,
}

impl UpdateState {
    /// Every state, for exhaustive tests.
    pub const ALL: [UpdateState; 7] = [
        UpdateState::Idle,
        UpdateState::Fetching,
        UpdateState::UpToDate,
        UpdateState::UpdateAvailable,
        UpdateState::Building,
        UpdateState::Installed,
        UpdateState::Failed,
    ];
}

/// Events that drive an update run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UpdateEvent {
    /// Begin: clone or fetch the source repository.
    Start,
    /// The fetched ref is the commit the binary was built from.
    FetchedSame,
    /// The fetched ref is a different commit (or the build commit is unknown).
    FetchedNewer,
    /// Cloning or fetching failed.
    FetchFailed,
    /// Start the release installer or `cargo install`.
    Build,
    /// `cargo install` succeeded.
    BuildSucceeded,
    /// `cargo install` failed.
    BuildFailed,
    /// Try again after a failure.
    Retry,
}

impl UpdateEvent {
    /// Every event, for exhaustive tests.
    pub const ALL: [UpdateEvent; 8] = [
        UpdateEvent::Start,
        UpdateEvent::FetchedSame,
        UpdateEvent::FetchedNewer,
        UpdateEvent::FetchFailed,
        UpdateEvent::Build,
        UpdateEvent::BuildSucceeded,
        UpdateEvent::BuildFailed,
        UpdateEvent::Retry,
    ];
}

use UpdateEvent as E;
use UpdateState as S;

/// The complete transition table. Pairs not listed are illegal.
pub const TRANSITIONS: &[(UpdateState, UpdateEvent, UpdateState)] = &[
    (S::Idle, E::Start, S::Fetching),
    (S::Fetching, E::FetchedSame, S::UpToDate),
    (S::Fetching, E::FetchedNewer, S::UpdateAvailable),
    (S::Fetching, E::FetchFailed, S::Failed),
    (S::UpdateAvailable, E::Build, S::Building),
    // `--force`: rebuild the same commit.
    (S::UpToDate, E::Build, S::Building),
    (S::Building, E::BuildSucceeded, S::Installed),
    (S::Building, E::BuildFailed, S::Failed),
    (S::Failed, E::Retry, S::Fetching),
];

/// Returns the next state for a legal `(state, event)` pair.
pub fn next(state: UpdateState, event: UpdateEvent) -> Option<UpdateState> {
    TRANSITIONS
        .iter()
        .find(|(from, on, _)| *from == state && *on == event)
        .map(|(_, _, to)| *to)
}

/// A rejected `(state, event)` pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IllegalTransition {
    pub state: UpdateState,
    pub event: UpdateEvent,
}

impl fmt::Display for IllegalTransition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "event {:?} is not allowed in state {:?}",
            self.event, self.state
        )
    }
}

impl std::error::Error for IllegalTransition {}

/// The update lifecycle. The state only changes through [`UpdateLifecycle::handle`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateLifecycle {
    state: UpdateState,
}

impl Default for UpdateLifecycle {
    fn default() -> Self {
        Self {
            state: UpdateState::Idle,
        }
    }
}

impl UpdateLifecycle {
    /// Current state.
    pub fn state(&self) -> UpdateState {
        self.state
    }

    /// Applies an event; illegal events are rejected and leave the state unchanged.
    pub fn handle(&mut self, event: UpdateEvent) -> Result<UpdateState, IllegalTransition> {
        let to = next(self.state, event).ok_or(IllegalTransition {
            state: self.state,
            event,
        })?;
        self.state = to;
        Ok(to)
    }
}

/// Where to fetch from and what to build.
#[derive(Debug, Clone)]
pub struct UpdateOptions {
    /// Private clone (`$WN_HOME/src`).
    pub src: PathBuf,
    /// Source repository URL or path.
    pub repo: String,
    /// Branch, tag or commit to build.
    pub git_ref: String,
    /// Only report whether an update is available.
    pub check_only: bool,
    /// Rebuild even when already up to date.
    pub force: bool,
    /// `cargo install --root` (default: cargo's own default).
    pub cargo_root: Option<PathBuf>,
    /// The cargo executable.
    pub cargo: PathBuf,
    /// Commit the running binary was built from (`None` when unknown).
    pub current: Option<String>,
    /// Stream cargo's progress to stderr.
    pub show_build_output: bool,
    /// Installer-owned release binary directory, when this executable carries the marker.
    pub release_install_dir: Option<PathBuf>,
}

impl UpdateOptions {
    /// Options from the environment, as `install.sh` sets them up.
    pub fn from_env(git_ref: &str) -> Self {
        Self {
            src: wn_home().join("src"),
            repo: std::env::var("WN_REPO_URL").unwrap_or_else(|_| DEFAULT_REPO.to_string()),
            git_ref: git_ref.to_string(),
            check_only: false,
            force: false,
            cargo_root: std::env::var_os("WN_BIN_ROOT").map(PathBuf::from),
            cargo: find_cargo(),
            current: (!BUILD_COMMIT.is_empty()).then(|| BUILD_COMMIT.to_string()),
            show_build_output: true,
            release_install_dir: std::env::current_exe().ok().and_then(|exe| {
                let dir = exe.parent()?;
                (std::fs::read_to_string(dir.join("wn.install-method"))
                    .ok()?
                    .trim()
                    == "release")
                    .then(|| dir.to_path_buf())
            }),
        }
    }
}

/// `$WN_HOME`, else `$XDG_DATA_HOME/where-next`, else `~/.local/share/where-next`.
pub fn wn_home() -> PathBuf {
    if let Some(h) = std::env::var_os("WN_HOME") {
        return PathBuf::from(h);
    }
    if let Some(x) = std::env::var_os("XDG_DATA_HOME") {
        return PathBuf::from(x).join("where-next");
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."));
    home.join(".local").join("share").join("where-next")
}

/// `$WN_CARGO`, else `cargo` on `PATH`, else `~/.cargo/bin/cargo`.
pub fn find_cargo() -> PathBuf {
    if let Some(c) = std::env::var_os("WN_CARGO") {
        return PathBuf::from(c);
    }
    let exe = if cfg!(windows) { "cargo.exe" } else { "cargo" };
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(exe);
            if p.is_file() {
                return p;
            }
        }
    }
    let home = std::env::var_os("CARGO_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")));
    home.map(|h| h.join("bin").join(exe))
        .unwrap_or_else(|| PathBuf::from(exe))
}

/// What happened, for text and `--json` output.
#[derive(Debug, Clone, Serialize)]
pub struct UpdateReport {
    pub state: UpdateState,
    /// Commit the running binary was built from.
    pub old: Option<String>,
    /// Commit of the requested ref.
    pub new: Option<String>,
    pub git_ref: String,
    pub src: PathBuf,
    pub message: String,
}

impl UpdateReport {
    /// Exit code: 0 up to date / installed, 10 update available (`--check`), 1 failed.
    pub fn exit_code(&self) -> i32 {
        match self.state {
            UpdateState::UpdateAvailable => EXIT_UPDATE_AVAILABLE,
            UpdateState::Failed => 1,
            _ => 0,
        }
    }
}

fn short(c: &Option<String>) -> String {
    match c {
        Some(c) => c.chars().take(7).collect(),
        None => "unknown".to_string(),
    }
}

/// Text form of a report.
pub fn render(r: &UpdateReport) -> String {
    if r.state == UpdateState::Installed
        && r.message.starts_with("installed the latest release binary")
    {
        return r.message.clone();
    }
    match r.state {
        UpdateState::UpToDate => format!("wn is up to date ({} on {})", short(&r.new), r.git_ref),
        UpdateState::UpdateAvailable => format!(
            "update available: {} -> {} ({})\nrun `wn update` to install it",
            short(&r.old),
            short(&r.new),
            r.git_ref
        ),
        UpdateState::Installed if !r.message.is_empty() => format!(
            "updated wn: {} -> {} ({})\nnote: {}",
            short(&r.old),
            short(&r.new),
            r.git_ref,
            r.message
        ),
        UpdateState::Installed => format!(
            "updated wn: {} -> {} ({})",
            short(&r.old),
            short(&r.new),
            r.git_ref
        ),
        _ => format!("wn update failed: {}", r.message),
    }
}

fn git_cmd(dir: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut cmd = Command::new("git");
    if let Some(d) = dir {
        cmd.arg("-C").arg(d);
    }
    let out = cmd
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("git not found ({e}); install git and retry"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Clones (first run) or fetches `git_ref`, returning the commit it points to.
fn fetch(opts: &UpdateOptions) -> Result<String, String> {
    if !opts.src.join(".git").exists() {
        if opts.src.exists()
            && std::fs::read_dir(&opts.src).map_or(true, |mut d| d.next().is_some())
        {
            return Err(format!(
                "{} exists but is not a git clone; remove it or set WN_HOME",
                opts.src.display()
            ));
        }
        if let Some(parent) = opts.src.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let src = opts.src.to_string_lossy().to_string();
        git_cmd(None, &["clone", "--quiet", &opts.repo, &src])?;
    } else {
        // Keep following the configured repository even if it moved.
        let _ = git_cmd(
            Some(&opts.src),
            &["remote", "set-url", "origin", &opts.repo],
        );
    }
    let fetched = git_cmd(
        Some(&opts.src),
        &["fetch", "--quiet", "--force", "origin", &opts.git_ref],
    );
    if fetched.is_err() {
        // A commit sha the server will not serve by name: fetch everything and resolve locally.
        git_cmd(
            Some(&opts.src),
            &["fetch", "--quiet", "--force", "--tags", "origin"],
        )?;
        let commit = format!("{}^{{commit}}", opts.git_ref);
        return git_cmd(Some(&opts.src), &["rev-parse", "--verify", &commit])
            .map_err(|_| format!("ref {:?} not found in {}", opts.git_ref, opts.repo));
    }
    git_cmd(Some(&opts.src), &["rev-parse", "FETCH_HEAD^{commit}"])
}

fn build(opts: &UpdateOptions, commit: &str) -> Result<(), String> {
    git_cmd(
        Some(&opts.src),
        &["checkout", "--quiet", "--force", "--detach", commit],
    )?;
    let mut cmd = Command::new(&opts.cargo);
    cmd.arg("install")
        .arg("--path")
        .arg(opts.src.join("crates").join("wn-cli"))
        .arg("--locked")
        .arg("--force");
    if let Some(root) = &opts.cargo_root {
        cmd.arg("--root").arg(root);
    }
    cmd.stdin(Stdio::null()).stdout(Stdio::null());
    if opts.show_build_output {
        cmd.stderr(Stdio::inherit());
    } else {
        cmd.stderr(Stdio::piped());
    }
    let out = cmd.output().map_err(|e| {
        format!(
            "cargo not found at {} ({e}); install Rust with `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh` and retry",
            opts.cargo.display()
        )
    })?;
    if out.status.success() {
        Ok(())
    } else {
        let tail: String = String::from_utf8_lossy(&out.stderr)
            .lines()
            .rev()
            .take(20)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>()
            .join("\n");
        Err(format!(
            "cargo install failed ({}){}",
            out.status,
            if tail.is_empty() {
                String::new()
            } else {
                format!(":\n{tail}")
            }
        ))
    }
}

/// Runs the update flow. `confirm` is asked before building (return `false` to stop).
pub fn run(opts: &UpdateOptions, confirm: &mut dyn FnMut(&UpdateReport) -> bool) -> UpdateReport {
    let mut life = UpdateLifecycle::default();
    let mut report = UpdateReport {
        state: UpdateState::Idle,
        old: opts.current.clone(),
        new: None,
        git_ref: opts.git_ref.clone(),
        src: opts.src.clone(),
        message: String::new(),
    };
    let step = |life: &mut UpdateLifecycle, report: &mut UpdateReport, event| {
        report.state = life
            .handle(event)
            .expect("update flow follows the transition table");
    };
    step(&mut life, &mut report, E::Start);
    if let Some(dir) = &opts.release_install_dir {
        report.git_ref = "release".into();
        report.old = Some(format!("v{}", env!("CARGO_PKG_VERSION")));
        let tag = match requested_release_tag() {
            Ok(tag) => tag,
            Err(e) => {
                report.message = e;
                step(&mut life, &mut report, E::FetchFailed);
                return report;
            }
        };
        let same = report.old.as_deref() == Some(tag.as_str());
        report.new = Some(tag.clone());
        step(
            &mut life,
            &mut report,
            if same {
                E::FetchedSame
            } else {
                E::FetchedNewer
            },
        );
        if opts.check_only || (same && !opts.force) {
            return report;
        }
        if !confirm(&report) {
            report.message = "cancelled".into();
            return report;
        }
        step(&mut life, &mut report, E::Build);
        match reinstall_release(dir, &tag) {
            Ok(()) => {
                step(&mut life, &mut report, E::BuildSucceeded);
                let notes = after_install(&dir.join(if cfg!(windows) { "wn.exe" } else { "wn" }));
                report.message = if notes.is_empty() {
                    "installed the latest release binary".into()
                } else {
                    format!("installed the latest release binary; {}", notes.join("; "))
                };
            }
            Err(e) => {
                report.message = e;
                step(&mut life, &mut report, E::BuildFailed);
            }
        }
        return report;
    }
    let target = match fetch(opts) {
        Ok(t) => t,
        Err(e) => {
            report.message = e;
            step(&mut life, &mut report, E::FetchFailed);
            return report;
        }
    };
    report.new = Some(target.clone());
    let same = opts.current.as_deref() == Some(target.as_str());
    step(
        &mut life,
        &mut report,
        if same {
            E::FetchedSame
        } else {
            E::FetchedNewer
        },
    );
    if opts.check_only || (same && !opts.force) {
        return report;
    }
    if !confirm(&report) {
        report.message = "cancelled".to_string();
        return report;
    }
    step(&mut life, &mut report, E::Build);
    match build(opts, &target) {
        Ok(()) => {
            step(&mut life, &mut report, E::BuildSucceeded);
            report.message = after_install(&installed_binary(opts)).join("; ");
        }
        Err(e) => {
            report.message = e;
            step(&mut life, &mut report, E::BuildFailed);
        }
    }
    report
}

fn requested_release_tag() -> Result<String, String> {
    let tag = match std::env::var("WN_VERSION") {
        Ok(tag) if tag != "latest" => tag,
        _ => {
            let output = Command::new("curl")
                .args([
                    "--proto",
                    "=https",
                    "--tlsv1.2",
                    "-fsSL",
                    "https://api.github.com/repos/andreylukin/where-next/releases/latest",
                ])
                .output()
                .map_err(|e| format!("could not find latest release: {e}"))?;
            if !output.status.success() {
                return Err(format!("could not find latest release: {}", output.status));
            }
            let release: serde_json::Value = serde_json::from_slice(&output.stdout)
                .map_err(|e| format!("invalid release response: {e}"))?;
            release["tag_name"]
                .as_str()
                .ok_or("latest release has no tag")?
                .to_string()
        }
    };
    if !tag.starts_with('v')
        || !tag[1..]
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        return Err(format!("invalid release tag: {tag}"));
    }
    Ok(tag)
}

fn reinstall_release(dir: &Path, tag: &str) -> Result<(), String> {
    let temp =
        tempfile::tempdir().map_err(|e| format!("could not create installer directory: {e}"))?;
    let downloaded = temp.path().join("install.sh");
    let script = if let Some(path) = std::env::var_os("WN_INSTALL_SCRIPT") {
        PathBuf::from(path)
    } else {
        let url =
            format!("https://raw.githubusercontent.com/andreylukin/where-next/{tag}/install.sh");
        let status = Command::new("curl")
            .args(["--proto", "=https", "--tlsv1.2", "-fsSL", &url, "-o"])
            .arg(&downloaded)
            .status()
            .map_err(|e| format!("could not download release installer: {e}"))?;
        if !status.success() {
            return Err(format!("could not download release installer: {status}"));
        }
        downloaded
    };
    let status = Command::new("sh")
        .arg(&script)
        .arg("--no-model")
        .env("WN_FROM", "release")
        .env("WN_VERSION", tag)
        .env("WN_INSTALL_DIR", dir)
        .status()
        .map_err(|e| format!("could not run release installer: {e}"));
    let status = status?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("release installer failed: {status}"))
    }
}

/// Where `cargo install` put the new `wn`: `--root`, else `$CARGO_INSTALL_ROOT`, `$CARGO_HOME` or
/// `~/.cargo`.
pub fn installed_binary(opts: &UpdateOptions) -> PathBuf {
    let exe = if cfg!(windows) { "wn.exe" } else { "wn" };
    let root = opts
        .cargo_root
        .clone()
        .or_else(|| std::env::var_os("CARGO_INSTALL_ROOT").map(PathBuf::from))
        .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".cargo"))
        })
        .unwrap_or_default();
    root.join("bin").join(exe)
}

/// After installing: stop a daemon from the old build and re-sync previously installed agent
/// skills, with the new binary. Failures don't fail the update; they come back as notes.
fn after_install(bin: &Path) -> Vec<String> {
    let steps: [(&[&str], &str); 2] = [
        (&["daemon", "stop"], "stopping the old daemon"),
        (
            &["skill", "sync", "--yes", "--from-state"],
            "re-syncing skills",
        ),
    ];
    let mut notes = Vec::new();
    for (args, what) in steps {
        let ok = Command::new(bin)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            notes.push(format!("{what} failed (run `wn {}`)", args.join(" ")));
        }
    }
    // Never download ~1 GB unasked from `wn update`: say when the default model's pin moved.
    let model = Command::new(bin)
        .args(["model", "pull", "--check"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    if model.is_ok_and(|s| s.code() == Some(EXIT_UPDATE_AVAILABLE)) {
        notes.push("the default model is missing or has an update: run `wn model pull`".into());
    }
    notes
}

/// Interactive confirmation on a terminal; `--yes` or no terminal proceeds without asking.
pub fn confirm_on_tty(yes: bool) -> impl FnMut(&UpdateReport) -> bool {
    move |r: &UpdateReport| {
        if yes || !std::io::stdin().is_terminal() {
            return true;
        }
        if r.git_ref == "release" {
            eprint!(
                "install wn release {} -> {}? [Y/n] ",
                short(&r.old),
                short(&r.new)
            );
        } else {
            eprint!(
                "rebuild wn {} -> {} from {} (takes a few minutes)? [Y/n] ",
                short(&r.old),
                short(&r.new),
                r.git_ref
            );
        }
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        !matches!(line.trim(), "n" | "N" | "no" | "No")
    }
}
