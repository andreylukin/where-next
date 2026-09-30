//! The `wn` command: index a repository, fit its personal adapter, and answer "where next?".
//!
//! Commands are thin wrappers over reports defined here so they can be tested without spawning
//! a process. Every report has a JSON form (`--json`) for agents and a short text form for people.

pub mod agents;
pub mod ask_text;
pub mod bench;
pub mod daemon;
pub mod hooks;
pub mod jsonedit;
pub mod progress;
pub mod report;
pub mod update;

use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde::Serialize;
use wn_core::adapter::{AdapterParams, ADAPTER_COMMITS, ADAPTER_REFIT_EVERY};
use wn_core::adapter_lifecycle::{AdapterEvent, AdapterLifecycle, AdapterState};
use wn_core::encoder::{Encoder, HashEncoder};
use wn_core::index::{EntryKind, Index, IndexedFile, RefreshStats};
use wn_core::index_lifecycle::IndexState;
use wn_core::rank::Outcome;
use wn_core::runtime::{
    fit_from_history, load_adapter, save_adapter, suggest_with_exact, HistoryExample,
    StoredAdapter, SuggestOptions,
};
use wn_daemon::indexer::Indexer;
use wn_git::{commits_since, history, scan, Coverage};
use wn_sources::{read_text, Kind, MAX_CONFIG_BYTES, MAX_SOURCE_BYTES};

#[cfg(feature = "onnx")]
pub mod models;
pub mod skill;
pub mod stats;
pub mod uninstall;

/// `wn --version`: the crate version plus the commit it was built from, e.g. `0.1.1 (abc1234 2026-09-29)`.
pub const VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), env!("WN_VERSION_SUFFIX"));

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "wn",
    version = VERSION,
    about = "Fast local \"where next\" hints for coding agents and developers",
    after_help = TOP_EXAMPLES
)]
pub struct Cli {
    /// Repository (or any directory inside it).
    #[arg(long, global = true, default_value = ".")]
    pub path: PathBuf,
    /// Print JSON instead of text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Model directory (default: `$WN_MODEL_DIR`, else the best installed model).
    #[arg(long, global = true)]
    pub model: Option<PathBuf>,
    /// Answer in this process instead of the background daemon (also `WN_NO_DAEMON=1`).
    #[arg(long, global = true)]
    pub no_daemon: bool,
    /// Allow a directory outside any git repository, or your home directory or / itself
    /// (refused by default: everything under it would be indexed).
    #[arg(long, global = true)]
    pub any_dir: bool,
    /// Color text output: auto (terminal only; honors NO_COLOR and CLICOLOR_FORCE),
    /// always or never.
    #[arg(long, global = true, value_enum, value_name = "WHEN", default_value_t = ask_text::ColorWhen::Auto)]
    pub color: ask_text::ColorWhen,
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

const TOP_EXAMPLES: &str = "\
Examples:
  wn model pull                            install the default model (once, ~1.2 GB)
  wn init                                  index this repository, fit its adapter from git history
  wn status                                which model answers (\"lexical fallback\" = no model installed)
  wn ask \"where is the retry logic for S3 upload timeouts\"
  wn bench                                 replay this repository's history: hit@1/3/10
  wn stats                                 did your agents use the hints? replay vs grep, speed
  wn setup                                 connect Claude Code / Codex / Cursor (skill + hooks)

Docs: https://github.com/andreylukin/where-next#quick-start";

const INIT_EXAMPLES: &str = "\
Indexes source and config files (untracked included, git-ignored excluded) with the best installed
model, or the lexical fallback when none is installed, then fits the per-repository adapter from
recent commits. Safe to re-run; only changed files are re-embedded. Large repositories take minutes
the first time; progress goes to stderr. Directories outside a git repository, your home directory
and / are refused unless you pass --any-dir.

Examples:
  wn init
  wn --path ~/src/project init
  wn --model ~/models/gemma-xl1 init       use a specific model directory";

const ASK_EXAMPLES: &str = "\
Good queries are self-contained: say what you are looking for, plus any error text.
Use rg/grep for exact strings and identifiers. Results are hints: open the files and check.
\"no confident hint\" means wn abstained (nothing above the calibrated threshold): use normal search.
Configuration queries can include one config file in the three-hint budget.

Examples:
  wn ask \"where are gitignore rules matched against paths\"          good: says what to find
  wn ask \"the other one\"                                            bad: nothing to match
  wn ask \"why does the upload fail\" --context-file error.txt        add the error or last tool output
  cargo test 2>&1 | wn ask \"fix the failing test\" --context-file -
  wn ask --json \"where is the config loaded\"                        state, files, adapter
  wn ask --start \"add rate limiting to the API\"                     task start; skipped below 3,000 files";

fn parse_hint_count(value: &str) -> Result<usize, String> {
    match value.parse::<usize>() {
        Ok(n @ 1..=3) => Ok(n),
        _ => Err(
            "-k accepts 1 to 3 hints (the agent output cap is 3 paths / about 250 tokens)".into(),
        ),
    }
}

const BENCH_EXAMPLES: &str = "\
Replays recent commits as new tasks (query = commit message, candidates = files in the parent
commit, adapter fitted only on earlier commits) and reports hit@1/3/10 and MRR for lexical search,
the model, and the model with this repository's adapter. Read-only.

Examples:
  wn bench                                 the newest 300 eligible commits
  wn bench --commits 1000 --json
  wn bench --contextbench ~/data/contextbench    the public benchmark (see benchmarks/contextbench.md)";

const REPORT_EXAMPLES: &str = "\
The report holds only numbers, fixed labels and buckets: no repository names, paths, file names,
commit messages or query text. You see all of it before anything is sent.

Examples:
  wn report --dry-run                      show the report and the issue link, post nothing
  wn report                                show it, then post it as a GitHub issue if you answer y
  wn --json report --dry-run               the report as JSON";

const STATS_EXAMPLES: &str = "\
Everything is computed locally from wn's usage log, git, and your agents' transcripts (Claude Code
and Codex, read-only; they never leave this machine). \"Agents used the hint\" follows each
`wn ask` to the agent's next tool calls: exact = it read, ran or edited a hinted file; near = a
file in the same directory or the hint's test/source pair; elsewhere = other files; no files =
it moved on.

Examples:
  wn stats                                 this repository, last 30 days
  wn stats --all                           every repository, one row each
  wn stats --share                         a redacted card: no repository names, paths or queries
  wn stats --share --svg wn-stats.svg      the same card as an image to post
  wn stats --no-agents                     skip reading agent transcripts";

const MCP_EXAMPLES: &str = "\
Most agents only need `wn setup` (skill + hooks), which calls `wn ask` directly. Use the MCP
server for clients that prefer tools; it answers for the repository it is started in.

Examples:
  claude mcp add where-next -- wn mcp      register with Claude Code
  wn --path ~/src/project mcp              serve a specific repository";

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Index the repository and fit its personal adapter from git history.
    #[command(after_help = INIT_EXAMPLES)]
    Init,
    /// Connect Claude Code, Codex and Cursor so hints reach them automatically (skill + hooks).
    ///
    /// Asks first; `wn setup --uninstall` removes everything it added.
    #[command(after_help = skill::SETUP_EXAMPLES)]
    Setup(skill::SyncArgs),
    /// Rank the files (and optionally functions) to open next for a task.
    #[command(after_help = ASK_EXAMPLES)]
    Ask {
        /// The task or request.
        query: String,
        /// File with recent context (conversation, last tool output); `-` reads stdin.
        #[arg(long)]
        context_file: Option<PathBuf>,
        /// Also rank functions (first call indexes definitions and can take minutes in large repos).
        #[arg(long)]
        functions: bool,
        /// Maximum total hints (1-3); use --functions to reserve one for a definition.
        #[arg(short, default_value_t = 3, value_parser = parse_hint_count)]
        k: usize,
        /// Do not apply the personal adapter.
        #[arg(long)]
        no_adapter: bool,
        /// Use the strict abstain thresholds (fewer, more precise answers).
        #[arg(long)]
        strict: bool,
        /// Never abstain.
        #[arg(long)]
        no_abstain: bool,
        /// A task-start hint: skipped in repositories with fewer than --start-min-files files.
        #[arg(long)]
        start: bool,
        /// Minimum source files for --start hints.
        #[arg(long, default_value_t = wn_core::rank::START_HINT_MIN_FILES)]
        start_min_files: usize,
        /// Do not record this query in the local usage log (see `wn report`).
        #[arg(long)]
        no_log: bool,
    },
    /// Show the index, coverage and adapter state.
    Status,
    /// Refit the personal adapter now.
    Train,
    /// Restore the previous adapter (or remove the only one).
    Rollback,
    /// Serve hints over MCP (stdio).
    #[command(after_help = MCP_EXAMPLES)]
    Mcp,
    /// Measure hit@k on this repository's own history (default) or on ContextBench.
    #[command(after_help = BENCH_EXAMPLES)]
    Bench {
        /// Replay this repository's past commits (the default).
        #[arg(long)]
        history: bool,
        /// Run ContextBench tasks from `<dir>/tasks.jsonl` instead (see benchmarks/contextbench.md).
        #[arg(long, value_name = "DIR")]
        contextbench: Option<PathBuf>,
        /// Newest eligible commits to score.
        #[arg(long, default_value_t = 300)]
        commits: usize,
        /// Earlier commits each adapter is fitted on.
        #[arg(long, default_value_t = 200)]
        train: usize,
        /// Commits scored per adapter fit.
        #[arg(long, default_value_t = 100)]
        step: usize,
        /// Count test files as gold too.
        #[arg(long)]
        with_tests: bool,
        /// Skip the personal adapter.
        #[arg(long)]
        no_adapter: bool,
    },
    /// Rebuild wn from the tip of main (or --ref) of its source repository.
    Update {
        /// Only report whether an update is available (exit code 10 when it is).
        #[arg(long)]
        check: bool,
        /// Branch, tag or commit to build.
        #[arg(long = "ref", default_value = "main")]
        git_ref: String,
        /// Do not ask before rebuilding.
        #[arg(long, short = 'y')]
        yes: bool,
        /// Rebuild even when already up to date.
        #[arg(long)]
        force: bool,
    },
    /// Share an anonymous usage report as a GitHub issue (shown first; asks before sending).
    #[command(after_help = REPORT_EXAMPLES)]
    Report {
        /// Show the report and the issue link without posting.
        #[arg(long)]
        dry_run: bool,
        /// Maintainers: validate a posted report (JSON file, or an issue body with a JSON block).
        #[arg(long, value_name = "FILE", hide = true)]
        validate: Option<PathBuf>,
        /// Maintainers: summarise a JSONL file of validated reports as Markdown.
        #[arg(long, value_name = "FILE", hide = true)]
        summarize: Option<PathBuf>,
    },
    /// How wn is doing: whether agents acted on its hints, a replay against grep, speed.
    #[command(after_help = STATS_EXAMPLES)]
    Stats {
        /// Every repository with logged usage, one row each.
        #[arg(long)]
        all: bool,
        /// Days to cover (usage is kept for 30).
        #[arg(long, default_value_t = 30)]
        days: u64,
        /// A redacted card to post: no repository names, paths or queries.
        #[arg(long)]
        share: bool,
        /// Also write the redacted card as an SVG image (implies --share).
        #[arg(long, value_name = "FILE")]
        svg: Option<PathBuf>,
        /// Do not read agent transcripts (also `WN_STATS_NO_AGENTS=1`).
        #[arg(long)]
        no_agents: bool,
    },
    /// Start, stop or inspect the background daemon that keeps models and indexes warm.
    Daemon {
        #[command(subcommand)]
        action: daemon::DaemonAction,
    },
    /// The where-next skill (`wn skill sync` is `wn setup`; `wn skill show` prints it).
    Skill {
        #[command(subcommand)]
        action: skill::SkillAction,
    },
    /// Agent hook entry points (run by the agents, not by hand).
    #[command(hide = true)]
    Hook {
        #[command(subcommand)]
        kind: hooks::HookKind,
    },
    /// Remove wn and everything it added: agent skill and hooks, caches, models, the binary.
    ///
    /// Shows the full list first and asks once.
    #[command(after_help = uninstall::UNINSTALL_EXAMPLES)]
    Uninstall(uninstall::UninstallArgs),
    /// Install, list or remove models.
    #[cfg(feature = "onnx")]
    Model {
        #[command(subcommand)]
        action: models::ModelAction,
    },
}

/// The user's home directory (where agents keep their settings).
pub fn skill_home() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Cache root: `$WHERE_NEXT_HOME`, else `~/.cache/where-next`.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("WHERE_NEXT_HOME") {
        return PathBuf::from(h);
    }
    user_cache().join("where-next")
}

fn user_cache() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join(".cache")
}

/// Where installed models live: `$WN_MODELS_HOME`, else `~/.cache/where-next-models`.
pub fn models_home() -> PathBuf {
    std::env::var_os("WN_MODELS_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_cache().join("where-next-models"))
}

/// Models tried in order when none is named: the best measured first.
pub const DEFAULT_MODELS: &[&str] = &["gemma-xl1", "gemma-g2r", "v2b"];

/// The model directory to use: `--model`, else `$WN_MODEL_DIR`, else the first installed
/// [`DEFAULT_MODELS`] entry under [`models_home`]. `None` means no model is installed.
pub fn resolve_model(explicit: Option<&Path>) -> Option<PathBuf> {
    let env = std::env::var_os("WN_MODEL_DIR").map(PathBuf::from);
    resolve_model_in(explicit, env.as_deref(), &models_home())
}

/// [`resolve_model`] with its inputs made explicit (for tests).
pub fn resolve_model_in(
    explicit: Option<&Path>,
    env_dir: Option<&Path>,
    models: &Path,
) -> Option<PathBuf> {
    explicit.or(env_dir).map(Path::to_path_buf).or_else(|| {
        DEFAULT_MODELS
            .iter()
            .map(|name| models.join(name))
            .find(|dir| dir.join("wn-model.json").is_file())
    })
}

/// Per-repository, per-model cache directory (shared with `wn mcp` and the MCP server).
pub fn repo_dir(root: &Path, fingerprint: &str) -> PathBuf {
    wn_daemon::workspace::model_cache_dir(&home(), root, fingerprint)
}

/// Fallback reason when no model is installed.
pub const NO_MODEL: &str = "no model installed";

/// Printed with hints and status when [`NO_MODEL`] applies.
pub const NO_MODEL_HINT: &str =
    "no model installed → run `wn model pull` (hints use the lexical fallback until then)";

/// Which encoder is in use.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EncoderInfo {
    /// Fingerprint of the encoder (vector cache key).
    pub fingerprint: String,
    /// True when the lexical fallback answers instead of a model.
    pub fallback: bool,
    /// Model name (from `wn-model.json`) when a model is loaded.
    pub model: Option<String>,
    /// Why the fallback is in use.
    pub reason: Option<String>,
}

/// A shareable encoder (the daemon reuses one per model across repositories).
pub type SharedEncoder = Arc<dyn Encoder + Send + Sync>;

/// The encoder to use: the verified model from [`resolve_model`], or the lexical fallback
/// when no model is installed or it fails verification (hints still work, and say so).
pub fn encoder(model: Option<&Path>) -> (SharedEncoder, EncoderInfo) {
    encoder_for(resolve_model(model).as_deref())
}

/// The encoder for an already resolved model directory (`None`: no model installed).
pub fn encoder_for(dir: Option<&Path>) -> (SharedEncoder, EncoderInfo) {
    let fallback = |reason: String| {
        let e = HashEncoder::default();
        let info = EncoderInfo {
            fingerprint: e.fingerprint(),
            fallback: true,
            model: None,
            reason: Some(reason),
        };
        (Arc::new(e) as SharedEncoder, info)
    };
    let Some(dir) = dir else {
        return fallback(NO_MODEL.into());
    };
    open_model(dir).unwrap_or_else(|e| fallback(format!("model unavailable: {e}")))
}

#[cfg(feature = "onnx")]
fn open_model(dir: &Path) -> Result<(SharedEncoder, EncoderInfo), String> {
    let e = wn_embed::core_encoder::OnnxEncoder::open(dir, None)?;
    let info = EncoderInfo {
        fingerprint: e.fingerprint(),
        fallback: false,
        model: Some(e.spec().name.clone()),
        reason: None,
    };
    Ok((Arc::new(e), info))
}

#[cfg(not(feature = "onnx"))]
fn open_model(_dir: &Path) -> Result<(SharedEncoder, EncoderInfo), String> {
    Err("built without the onnx feature".into())
}

/// Where `path` sits, for deciding what `wn` may index.
enum Place {
    /// Inside an ordinary git repository (its work tree root).
    Repo(PathBuf),
    /// Not inside any git repository.
    NotGit(PathBuf),
    /// Exactly a git repository at `$HOME` or `/` (named by the `&str`).
    Broad(PathBuf, &'static str),
    /// Below a git repository at `$HOME` or `/`: the path itself, and the broad repository.
    BelowBroad(PathBuf, PathBuf, &'static str),
}

fn place(path: &Path, home: Option<&Path>) -> Place {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let toplevel = std::process::Command::new("git")
        .arg("-C")
        .arg(path)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty());
    let here = canon(path);
    let Some(top) = toplevel.map(|t| canon(Path::new(&t))) else {
        return Place::NotGit(here);
    };
    let broad = if top.parent().is_none() {
        Some("the filesystem root")
    } else if home.is_some_and(|h| canon(h) == top) {
        Some("your home directory")
    } else {
        None
    };
    match broad {
        None => Place::Repo(top),
        Some(what) if here == top => Place::Broad(top, what),
        Some(what) => Place::BelowBroad(here, top, what),
    }
}

/// The directory `wn` indexes for `path`: its git work tree root, except that a git repository at
/// `$HOME` or `/` (dotfiles) never stands in for a directory below it (that directory is used).
pub fn project_root(path: &Path) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match place(path, home.as_deref()) {
        Place::Repo(root) | Place::NotGit(root) | Place::Broad(root, _) => root,
        Place::BelowBroad(here, _, _) => here,
    }
}

/// The repository `wn` would index for `path` (see [`project_root`]). Refuses (with a message
/// naming git) a directory outside any git repository, and `home` or `/` itself or a directory
/// whose only repository is one of them, unless `any_dir` (`--any-dir`) is set.
pub fn check_repo(path: &Path, any_dir: bool, home: Option<&Path>) -> Result<PathBuf, String> {
    let place = place(path, home);
    if any_dir {
        return Ok(match place {
            Place::Repo(root) | Place::NotGit(root) | Place::Broad(root, _) => root,
            Place::BelowBroad(here, _, _) => here,
        });
    }
    match place {
        Place::Repo(root) => Ok(root),
        Place::NotGit(here) => Err(format!(
            "wn: {} is not inside a git repository, so there is nothing to index.\n\
             Run wn inside a git repository (or pass --path <repo>); \
             --any-dir indexes this directory anyway.",
            here.display()
        )),
        Place::Broad(root, what) => Err(format!(
            "wn: refusing to index {} ({what}): it is a git repository, but indexing everything \
             under it would take a long time.\nRun wn inside a project repository; --any-dir \
             indexes it anyway.",
            root.display()
        )),
        Place::BelowBroad(here, root, what) => Err(format!(
            "wn: {} is not inside a project repository: the nearest one is {} ({what}), and \
             indexing all of it would take a long time.\nRun wn inside a project repository; \
             --any-dir indexes just this directory.",
            here.display(),
            root.display()
        )),
    }
}

/// Lists the repository's indexable files with their version ids (git listing and stat calls
/// only; safe to run without holding any lock).
pub fn scan_repo(root: &Path) -> (Vec<IndexedFile>, Coverage) {
    let (files, coverage) = scan(root);
    let list = files
        .into_iter()
        .map(|(path, f)| IndexedFile {
            path,
            cid: f.id.as_str().to_string(),
            kind: f.kind,
        })
        .collect();
    (list, coverage)
}

/// A repository opened for one command.
pub struct Workspace {
    /// Repository root.
    pub root: PathBuf,
    /// Cache directory for this repository and model.
    pub dir: PathBuf,
    /// Encoder.
    pub encoder: SharedEncoder,
    /// Encoder description.
    pub info: EncoderInfo,
    /// Vector index.
    pub index: Index,
    /// Adapter lifecycle.
    pub adapter_life: AdapterLifecycle,
    /// Current adapter, if any.
    pub adapter: Option<StoredAdapter>,
    /// Last scan's coverage.
    pub coverage: Coverage,
    /// This repository's indexer: one per repository across processes (see
    /// [`wn_daemon::indexer`]).
    pub indexer: Indexer,
    /// Where progress of long index builds goes (`None`: nowhere).
    pub progress: Option<Arc<dyn progress::Sink>>,
    /// The stored revision (see [`wn_daemon::indexer`]) this copy of the index reflects.
    revision: String,
}

impl Workspace {
    /// Opens the repository containing `path` with the model from [`resolve_model`].
    pub fn open(path: &Path, model: Option<&Path>) -> Workspace {
        let (encoder, info) = encoder(model);
        Workspace::open_with(path, encoder, info)
    }

    /// Opens the repository containing `path` with an already loaded encoder.
    pub fn open_with(path: &Path, encoder: SharedEncoder, info: EncoderInfo) -> Workspace {
        Workspace::open_in(path, &home(), encoder, info)
    }

    /// [`Workspace::open_with`] with an explicit cache home.
    pub fn open_in(
        path: &Path,
        cache_home: &Path,
        encoder: SharedEncoder,
        info: EncoderInfo,
    ) -> Workspace {
        let root = project_root(path);
        let dir = wn_daemon::workspace::model_cache_dir(cache_home, &root, &info.fingerprint);
        let mut ws = Workspace {
            indexer: Indexer::new(&dir),
            index: Index::open(&dir.join("index"), &info.fingerprint),
            root,
            dir,
            encoder,
            info,
            adapter_life: AdapterLifecycle::default(),
            adapter: None,
            coverage: Coverage::default(),
            progress: None,
            revision: String::new(),
        };
        ws.load_adapter();
        ws
    }

    fn load_adapter(&mut self) {
        self.adapter = load_adapter(&self.dir.join("adapter"));
        self.adapter_life = AdapterLifecycle::starting_in(match &self.adapter {
            Some(a) if a.meta.base == self.info.fingerprint => AdapterState::Active,
            Some(_) => AdapterState::Invalidated,
            None => AdapterState::None,
        });
    }

    /// Runs `work` as this repository's only indexer. `wait: false` returns `Ok(None)` at once
    /// when another indexer (another process, or another thread's workspace) is busy; otherwise
    /// this waits for it, then reloads whatever it stored so nothing is embedded twice.
    fn exclusive<T>(
        &mut self,
        wait: bool,
        work: impl FnOnce(&mut Self, &progress::Tracker) -> Result<(T, bool), String>,
    ) -> Result<Option<T>, String> {
        let name = self
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| self.root.display().to_string());
        let tracker = progress::Tracker::new(&name);
        let _ticker = self
            .progress
            .clone()
            .map(|sink| progress::Ticker::start(tracker.clone(), sink));
        let dir = self.dir.display().to_string();
        let unusable =
            |e: std::io::Error| format!("cannot write to the cache directory {dir}: {e}");
        let waited = if wait {
            self.indexer
                .begin(|| tracker.phase(progress::Phase::Waiting))
                .map_err(unusable)?
        } else if self.indexer.try_begin().map_err(unusable)? {
            false
        } else {
            self.indexer.give_up();
            return Ok(None);
        };
        if waited || self.indexer.revision() != self.revision {
            // Another indexer stored a newer index (and maybe an adapter) meanwhile.
            self.revision = self.indexer.revision();
            self.index = Index::open(&self.dir.join("index"), &self.info.fingerprint);
            self.load_adapter();
        }
        tracker.phase(progress::Phase::Scanning);
        let result = work(self, &tracker);
        // A failed build may still have stored a checkpoint.
        let changed = result.as_ref().map_or(true, |(_, changed)| *changed);
        if changed && self.indexer.mark_changed().is_ok() {
            self.revision = self.indexer.revision();
        }
        self.indexer.finish(result.is_ok());
        result.map(|(value, _)| Some(value))
    }

    /// Scans the repository and brings the index up to date (waiting for another indexer of
    /// this repository if one is busy).
    pub fn refresh(&mut self, with_functions: bool) -> Result<RefreshStats, String> {
        self.exclusive(true, |ws, tracker| {
            let (list, coverage) = scan_repo(&ws.root);
            ws.apply_locked(list, coverage, with_functions, tracker)
                .map(|s| (s, s.encoded > 0 || s.removed > 0))
        })
        .map(Option::unwrap_or_default)
    }

    /// Brings the index up to date with a scan taken earlier without any lock. `Ok(None)`: another
    /// indexer of this repository is busy, so nothing was done (try again later).
    pub fn try_apply_scan(
        &mut self,
        list: Vec<IndexedFile>,
        coverage: Coverage,
        with_functions: bool,
    ) -> Result<Option<RefreshStats>, String> {
        self.exclusive(false, |ws, tracker| {
            ws.apply_locked(list, coverage, with_functions, tracker)
                .map(|s| (s, s.encoded > 0 || s.removed > 0))
        })
    }

    fn apply_locked(
        &mut self,
        list: Vec<IndexedFile>,
        coverage: Coverage,
        with_functions: bool,
        tracker: &progress::Tracker,
    ) -> Result<RefreshStats, String> {
        self.coverage = coverage;
        let root = self.root.clone();
        let read = move |p: &str, kind: Kind| {
            tracker.read_one();
            let max = if kind == Kind::Source {
                MAX_SOURCE_BYTES
            } else {
                MAX_CONFIG_BYTES
            };
            read_text(&root.join(p), max).ok()
        };
        let encoder = progress::Counting {
            inner: self.encoder.as_ref(),
            tracker,
        };
        self.index
            .refresh(&list, &read, &encoder, with_functions)
            .map_err(|e| e.to_string())
    }

    /// Whether the adapter should be (re)fitted: none, fitted for another model, or enough new
    /// commits since its history cutoff.
    pub fn needs_fit(&self) -> bool {
        match &self.adapter {
            None => true,
            Some(a) if a.meta.base != self.info.fingerprint => true,
            Some(a) => {
                let since = a
                    .meta
                    .history_cutoff
                    .as_deref()
                    .and_then(|sha| commits_since(&self.root, sha));
                since.map_or(true, |n| n >= ADAPTER_REFIT_EVERY)
            }
        }
    }

    /// Fits the adapter from history, keeping the previous one for `rollback` (as this
    /// repository's only indexer).
    pub fn fit(&mut self) -> Result<String, String> {
        self.exclusive(true, |ws, tracker| {
            tracker.phase(progress::Phase::Fitting);
            ws.fit_locked().map(|msg| (msg, true))
        })
        .map(Option::unwrap_or_default)
    }

    /// `wn rollback`: restores the previous adapter, or removes the only one (as this
    /// repository's only indexer, so it never races a fit).
    pub fn rollback(&mut self) -> Result<String, String> {
        self.exclusive(true, |ws, _| {
            let dir = ws.dir.join("adapter");
            let prev = ws.dir.join("adapter.prev");
            let msg = if prev.exists() {
                let _ = std::fs::remove_dir_all(&dir);
                match std::fs::rename(&prev, &dir) {
                    Ok(()) => "adapter: restored the previous adapter".to_string(),
                    Err(e) => format!("adapter: rollback failed: {e}"),
                }
            } else if dir.exists() {
                let _ = std::fs::remove_dir_all(&dir);
                let _ = ws.adapter_life.handle(AdapterEvent::Discard);
                "adapter: removed (queries use the base model until the next fit)".to_string()
            } else {
                return Ok(("adapter: nothing to roll back".to_string(), false));
            };
            ws.load_adapter();
            Ok((msg, true))
        })
        .map(Option::unwrap_or_default)
    }

    fn fit_locked(&mut self) -> Result<String, String> {
        let _ = self.adapter_life.handle(AdapterEvent::Fit);
        let commits: Vec<HistoryExample> = history(&self.root, ADAPTER_COMMITS)
            .into_iter()
            .map(|c| HistoryExample {
                sha: c.sha,
                date: c.date,
                subject: c.subject,
                body: c.body,
                paths: c.paths,
            })
            .collect();
        match fit_from_history(
            &self.index,
            &commits,
            &[],
            self.encoder.as_ref(),
            &AdapterParams::default(),
            ADAPTER_COMMITS,
            0,
        ) {
            Ok(a) => {
                let dir = self.dir.join("adapter");
                let prev = self.dir.join("adapter.prev");
                if dir.exists() {
                    let _ = std::fs::remove_dir_all(&prev);
                    copy_dir(&dir, &prev).map_err(|e| e.to_string())?;
                }
                save_adapter(&dir, &a).map_err(|e| e.to_string())?;
                let msg = format!(
                    "fitted on {} commits (revision {})",
                    a.meta.n_train, a.meta.revision
                );
                self.adapter = Some(a);
                let _ = self.adapter_life.handle(AdapterEvent::FitDone);
                Ok(msg)
            }
            Err(skip) => {
                let _ = self.adapter_life.handle(AdapterEvent::FitFailed);
                Err(skip.to_string())
            }
        }
    }
}

fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        std::fs::copy(entry.path(), to.join(entry.file_name()))?;
    }
    Ok(())
}

/// What `wn status` (and `wn init`) report.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    /// Repository root.
    pub repo: String,
    /// Index lifecycle state.
    pub index_state: String,
    /// Indexed source files.
    pub files: usize,
    /// Indexed config files.
    pub configs: usize,
    /// Indexed definitions.
    pub functions: usize,
    /// Scan coverage.
    pub coverage: Coverage,
    /// Adapter state.
    pub adapter_state: String,
    /// Adapter revision, when present.
    pub adapter_revision: Option<String>,
    /// Commits the adapter was fitted on.
    pub adapter_examples: Option<usize>,
    /// Outcome of the last fit attempt in this command, if any.
    pub adapter_note: Option<String>,
    /// Encoder in use.
    pub encoder: EncoderInfo,
}

fn status_of(ws: &Workspace, note: Option<String>) -> StatusReport {
    StatusReport {
        repo: ws.root.to_string_lossy().into_owned(),
        index_state: format!("{:?}", ws.index.state()),
        files: ws.index.count(EntryKind::File),
        configs: ws.index.count(EntryKind::Config),
        functions: ws.index.count(EntryKind::Function),
        coverage: ws.coverage.clone(),
        adapter_state: format!("{:?}", ws.adapter_life.state()),
        adapter_revision: ws.adapter.as_ref().map(|a| a.meta.revision.clone()),
        adapter_examples: ws.adapter.as_ref().map(|a| a.meta.n_train),
        adapter_note: note,
        encoder: ws.info.clone(),
    }
}

/// Text form of a status report.
pub fn render_status(s: &StatusReport) -> String {
    let name = Path::new(&s.repo)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut lines = vec![format!(
        "where-next: {}, {}{} indexed in {name} ({})",
        count(s.files, "source file"),
        count(s.configs, "config file"),
        if s.functions > 0 {
            format!(", {}", count(s.functions, "function"))
        } else {
            String::new()
        },
        s.index_state.to_lowercase()
    )];
    if s.coverage.unsupported > 0 {
        let exts: Vec<String> = s
            .coverage
            .unsupported_ext
            .iter()
            .map(|(e, n)| format!("{e} {n}"))
            .collect();
        lines.push(format!(
            "not indexed: {} ({})",
            count(s.coverage.unsupported, "file"),
            exts.join(", ")
        ));
    }
    let adapter = match (&s.adapter_revision, s.adapter_state.as_str()) {
        (Some(rev), "Active") => format!(
            "adapter: active, fitted on {} commits (revision {rev})",
            s.adapter_examples.unwrap_or(0)
        ),
        (Some(_), state) => format!("adapter: {}", state.to_lowercase()),
        (None, _) => "adapter: none".to_string(),
    };
    lines.push(match &s.adapter_note {
        Some(note) if s.adapter_revision.is_none() => format!("{adapter} ({note})"),
        _ => adapter,
    });
    lines.push(match (&s.encoder.model, &s.encoder.reason) {
        (Some(name), _) => format!("model: {name} ({})", s.encoder.fingerprint),
        (None, reason) => format!(
            "model: lexical fallback ({})",
            fallback_why(reason.as_deref())
        ),
    });
    lines.join("\n")
}

fn count(n: usize, noun: &str) -> String {
    format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
}

/// Why the lexical fallback answers, with what to do about it.
fn fallback_why(reason: Option<&str>) -> String {
    match reason.unwrap_or(NO_MODEL) {
        NO_MODEL => format!("{NO_MODEL}; run `wn model pull`"),
        reason => format!("{reason}; check --model / $WN_MODEL_DIR, or run `wn model pull`"),
    }
}

fn read_context(path: &Option<PathBuf>) -> String {
    match path {
        None => String::new(),
        Some(p) if p.as_os_str() == "-" => {
            let mut s = String::new();
            let _ = std::io::stdin().read_to_string(&mut s);
            s
        }
        Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
    }
}

/// Runs a parsed command, returning the text to print and the exit code.
pub fn run(cli: Cli) -> (String, i32) {
    // `ask` text is styled wherever it is built (here or in the daemon); strip it here.
    let styled = matches!(cli.command, Command::Ask { .. }) && !cli.json;
    let color = ask_text::color_enabled(cli.color);
    let (text, code) = run_command(cli);
    if styled {
        (ask_text::finish(text, color), code)
    } else {
        (text, code)
    }
}

fn run_command(cli: Cli) -> (String, i32) {
    if matches!(
        cli.command,
        Command::Init
            | Command::Status
            | Command::Train
            | Command::Rollback
            | Command::Ask { .. }
            | Command::Mcp
            | Command::Bench { .. }
            | Command::Daemon { .. }
            | Command::Stats { .. }
            | Command::Report {
                validate: None,
                summarize: None,
                ..
            }
            | Command::Hook { .. }
    ) {
        if let Err(e) =
            wn_daemon::usage::prepare_home(&home(), std::env::var_os("WHERE_NEXT_HOME").is_none())
        {
            if matches!(cli.command, Command::Hook { .. }) {
                return (String::new(), 0);
            }
            return (format!("wn: cannot prepare cache home: {e}"), 1);
        }
    }
    let uses_repo = matches!(
        cli.command,
        Command::Init
            | Command::Status
            | Command::Train
            | Command::Rollback
            | Command::Ask { .. }
            | Command::Mcp
            | Command::Bench { .. }
            | Command::Stats { all: false, .. }
    );
    if uses_repo && !cli.path.exists() {
        return (
            format!("wn: --path {} does not exist", cli.path.display()),
            2,
        );
    }
    // `wn stats` only reads logs; everything else would index the directory. `wn mcp` checks
    // too, but fails open per call instead of exiting (see `serve_mcp`).
    if uses_repo && !matches!(cli.command, Command::Stats { .. } | Command::Mcp) {
        let home = std::env::var_os("HOME").map(PathBuf::from);
        if let Err(msg) = check_repo(&cli.path, cli.any_dir, home.as_deref()) {
            return (msg, 2);
        }
    }
    if let Command::Mcp = cli.command {
        return serve_mcp(&cli);
    }
    if let Command::Bench { .. } = cli.command {
        return run_bench(cli);
    }
    if let Command::Stats {
        all,
        days,
        share,
        svg,
        no_agents,
    } = &cli.command
    {
        let opts = StatsArgs {
            all: *all,
            days: *days,
            share: *share || svg.is_some(),
            svg: svg.clone(),
            no_agents: *no_agents,
        };
        return run_stats(&cli, &opts);
    }
    if let Command::Update {
        check,
        git_ref,
        yes,
        force,
    } = &cli.command
    {
        let mut opts = update::UpdateOptions::from_env(git_ref);
        opts.check_only = *check;
        opts.force = *force;
        opts.show_build_output = !cli.json;
        let report = update::run(&opts, &mut update::confirm_on_tty(*yes));
        let code = report.exit_code();
        let text = if cli.json {
            erased::Json::to_json(&report)
        } else {
            update::render(&report)
        };
        return (text, code);
    }
    if let Command::Report {
        validate: Some(file),
        ..
    } = &cli.command
    {
        let text = std::fs::read_to_string(file).unwrap_or_default();
        let json = report::extract_json(&text).unwrap_or(text.trim());
        return match report::validate(json) {
            Ok(r) => (serde_json::to_string(&r).unwrap_or_default(), 0),
            Err(e) => (format!("invalid report: {e}"), 1),
        };
    }
    if let Command::Report {
        summarize: Some(file),
        ..
    } = &cli.command
    {
        let reports: Vec<report::UsageReport> = std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let v: serde_json::Value = serde_json::from_str(l).ok()?;
                report::validate(&v.get("report").unwrap_or(&v).to_string()).ok()
            })
            .collect();
        return (report::summarize(&reports), 0);
    }
    if let Command::Report { dry_run, .. } = cli.command {
        let model = resolve_model(cli.model.as_deref());
        let system = report::System::detect(model.as_deref());
        let collected = report::collect_from(&home(), &system, wn_daemon::usage::now());
        let opts = report::ReportOptions {
            json: cli.json,
            dry_run,
        };
        let state = report::run_flow(&collected, opts, &mut report::TerminalIo);
        let code = i32::from(state == report::ReportState::Failed);
        return (String::new(), code);
    }
    #[cfg(feature = "onnx")]
    if let Command::Model { action } = cli.command {
        let (report, code) = models::run(action, &models_home());
        let text = if cli.json {
            erased::Json::to_json(&report)
        } else {
            models::render(&report)
        };
        return (text, code);
    }
    if let Command::Daemon { action } = &cli.command {
        return daemon::run_action(action, &cli);
    }
    if let Command::Skill { action } = &cli.command {
        return skill::run(action, &cli);
    }
    if let Command::Setup(args) = &cli.command {
        return skill::run_sync(args, &cli);
    }
    if let Command::Uninstall(args) = &cli.command {
        return uninstall::run(args, cli.json);
    }
    if let Command::Hook { kind } = &cli.command {
        // Fail open: a hook always exits 0 and prints nothing unless it has hints.
        let mut input = String::new();
        let _ = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input);
        return (hooks::run(*kind, &input, &hooks::Deps::from_env()), 0);
    }
    let json = cli.json;
    // `ask` and `status` go through the background daemon (warm model and index) when it is
    // available; any failure falls back to answering in this process with identical output.
    let context = match &cli.command {
        Command::Ask { context_file, .. } => read_context(context_file),
        _ => String::new(),
    };
    if let Some(op) = daemon::op_for(&cli.command) {
        if let Some(out) = daemon::client::call(&cli, op, &context) {
            return out;
        }
    }
    let render: Arc<dyn progress::Sink> = Arc::new(progress::Render::stderr());
    let loading = progress::Ticker::step("loading the model", render.clone());
    let mut ws = Workspace::open(&cli.path, cli.model.as_deref());
    drop(loading);
    ws.progress = Some(render);
    let out = |value: &dyn erased::Json, text: String| if json { value.to_json() } else { text };
    match cli.command {
        Command::Init => {
            let out = status_command(&mut ws, StatusKind::Init, json);
            if !json && out.1 == 0 && !skill::connected(&skill_home(), Some(&ws.root)) {
                eprintln!("{}", skill::CONNECT_HINT);
            }
            out
        }
        Command::Status => status_command(&mut ws, StatusKind::Status, json),
        Command::Train => status_command(&mut ws, StatusKind::Train, json),
        Command::Ask {
            query,
            context_file: _,
            functions,
            k,
            no_adapter,
            strict,
            no_abstain,
            start,
            start_min_files,
            no_log,
        } => {
            let args = AskArgs {
                query,
                functions,
                k,
                no_adapter,
                strict,
                no_abstain,
                start,
                start_min_files,
                no_log,
            };
            ask_command(&mut ws, &args, &context, json)
        }
        Command::Rollback => {
            let msg = match ws.rollback() {
                Ok(msg) => msg,
                Err(e) => return (format!("wn rollback: {e}"), 1),
            };
            let value = serde_json::json!({ "message": msg });
            (out(&value, msg.clone()), 0)
        }
        Command::Mcp
        | Command::Bench { .. }
        | Command::Update { .. }
        | Command::Report { .. }
        | Command::Stats { .. }
        | Command::Daemon { .. }
        | Command::Skill { .. }
        | Command::Setup(_)
        | Command::Uninstall(_)
        | Command::Hook { .. } => {
            unreachable!("handled above")
        }
        #[cfg(feature = "onnx")]
        Command::Model { .. } => unreachable!("handled above"),
    }
}

/// `wn stats` options.
#[derive(Debug, Clone)]
pub struct StatsArgs {
    pub all: bool,
    pub days: u64,
    pub share: bool,
    pub svg: Option<PathBuf>,
    pub no_agents: bool,
}

/// `wn stats`: text or JSON, and the exit code.
pub fn run_stats(cli: &Cli, opts: &StatsArgs) -> (String, i32) {
    let now = wn_daemon::usage::now();
    let usage = wn_daemon::usage::load_all(&home(), now);
    let root = (!opts.all).then(|| stats::repo_root_of(&cli.path));
    let read_agents = !opts.no_agents && std::env::var_os(agents::OPT_OUT_ENV).is_none();
    let days = opts.days.clamp(1, wn_daemon::usage::RETENTION_DAYS);
    let since = now.saturating_sub(days * 86_400);
    // Match calls against every repository's answers, so a call is never pinned on the wrong one.
    let score = read_agents.then(|| agents::collect(&agents::Roots::detect(), &usage, since));
    let Some(mut stats) = stats::build(
        &usage,
        root.as_deref(),
        score.as_ref(),
        now,
        opts.days,
        &report::git_edited,
    ) else {
        let msg = if !wn_daemon::usage::enabled() {
            "wn stats: usage logging is off (WN_NO_LOG), so there is nothing to show".to_string()
        } else if let Some(root) = &root {
            let name = root
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            format!(
                "wn stats: nothing logged for {name} yet: run `wn init`, then `wn ask \"where is …\"`\n\
                 (`wn stats --all` shows every repository)"
            )
        } else {
            "wn stats: nothing logged yet: run `wn init` in a repository, then `wn ask \"where is …\"`"
                .to_string()
        };
        return (msg, 0);
    };
    let installed = skill::connected(&skill_home(), root.as_deref());
    stats::add_hooks(&mut stats, &hooks::load_runs(&home(), since), installed);
    let card = opts.share.then(|| stats::ShareCard::from_stats(&stats));
    let mut text = match (&card, cli.json) {
        (Some(c), true) => erased::Json::to_json(c),
        (Some(c), false) => stats::render_share(c),
        (None, true) => erased::Json::to_json(&stats),
        (None, false) => {
            let color = ask_text::color_enabled(cli.color);
            stats::render(&stats, stats::Style { color })
        }
    };
    if let (Some(path), Some(c)) = (&opts.svg, &card) {
        if let Err(e) = std::fs::write(path, stats::render_svg(c)) {
            return (
                format!("wn stats: could not write {}: {e}", path.display()),
                1,
            );
        }
        if !cli.json {
            text.push_str(&format!(
                "\n\nwrote {} (redacted: no repository names, paths or queries)",
                path.display()
            ));
        }
    }
    (text, 0)
}

/// The repository's cache directory (holds the usage log shared by all models).
fn usage_dir(ws: &Workspace) -> Option<&Path> {
    ws.dir.parent()
}

fn record_query(ws: &Workspace, query: &str, context: &str, outcome: &Outcome, ms: u128) {
    let Some(dir) = usage_dir(ws) else { return };
    let event = wn_daemon::usage::QueryEvent {
        ts: wn_daemon::usage::now(),
        kind: wn_core::rank::QueryKind::classify(query, context)
            .as_str()
            .to_string(),
        state: wn_daemon::daemon::state_label(outcome.state).to_string(),
        ms: u64::try_from(ms).unwrap_or(u64::MAX),
        model: ws.info.fingerprint.clone(),
        adapter: outcome.adapter.applied,
        files: ws.index.count(EntryKind::File),
        hinted: outcome.hints.files.iter().map(|h| h.path.clone()).collect(),
    };
    let _ = wn_daemon::usage::record_query(dir, &ws.root, &event);
}

fn record_index(ws: &Workspace, ms: u64) {
    let Some(dir) = usage_dir(ws) else { return };
    let mut extensions = std::collections::BTreeMap::new();
    for entry in ws.index.matrix(EntryKind::File).0 {
        let ext = Path::new(&entry.path)
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        *extensions.entry(ext).or_insert(0usize) += 1;
    }
    let event = wn_daemon::usage::IndexEvent {
        ts: wn_daemon::usage::now(),
        ms,
        files: ws.index.count(EntryKind::File),
        configs: ws.index.count(EntryKind::Config),
        extensions,
        history_commits: ws.adapter.as_ref().map_or(0, |a| a.meta.n_train),
        peak_mb: peak_memory_mb(),
        model: ws.info.fingerprint.clone(),
    };
    let _ = wn_daemon::usage::record_index(dir, &ws.root, &event);
}

fn record_bench(ws: &Workspace, report: &bench::Report) {
    let Some(dir) = usage_dir(ws) else { return };
    let find = |rows: &[bench::Score], prefix: &str| {
        rows.iter()
            .find(|s| s.method.starts_with(prefix))
            .map(|s| [s.hit1, s.hit3, s.hit10])
    };
    let rows = if report.matched.is_empty() {
        &report.all
    } else {
        &report.matched
    };
    let (Some(lexical), Some(model_hits)) = (find(rows, "lexical"), find(rows, "model")) else {
        return;
    };
    let event = wn_daemon::usage::BenchEvent {
        ts: wn_daemon::usage::now(),
        model: ws.info.fingerprint.clone(),
        files_median: report.files_median,
        tasks: if report.matched.is_empty() {
            report.evaluated
        } else {
            report.adapted
        },
        lexical,
        model_hits,
        adapter_hits: find(&report.matched, "model + adapter"),
    };
    let _ = wn_daemon::usage::record_bench(dir, &ws.root, &event);
}

/// Peak (Linux: high-water mark) or current (macOS) resident memory of this process, in MB.
fn peak_memory_mb() -> Option<u64> {
    if cfg!(target_os = "linux") {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let kb: u64 = status
            .lines()
            .find(|l| l.starts_with("VmHWM:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        Some(kb / 1024)
    } else if cfg!(target_os = "macos") {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()?;
        let kb: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(kb / 1024)
    } else {
        None
    }
}

/// Which status-style command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
pub enum StatusKind {
    /// Index and fit the adapter when it is missing or stale.
    Init,
    /// Index and report.
    Status,
    /// Index and refit the adapter now.
    Train,
}

/// `wn init | status | train` on an opened workspace: text or JSON, and the exit code. Shared by
/// the in-process path and the daemon so both print the same thing.
pub fn status_command(ws: &mut Workspace, kind: StatusKind, json: bool) -> (String, i32) {
    let mut note = None;
    let started = std::time::Instant::now();
    if let Err(e) = ws.refresh(false) {
        return (format!("wn: could not index {}: {e}", ws.root.display()), 1);
    }
    let index_ms = started.elapsed().as_millis();
    let should_fit = match kind {
        StatusKind::Train => true,
        StatusKind::Init => ws.needs_fit(),
        StatusKind::Status => false,
    };
    if should_fit && ws.index.state() == IndexState::Ready {
        note = Some(ws.fit().unwrap_or_else(|e| e));
    }
    if kind == StatusKind::Init && ws.index.state() == IndexState::Ready {
        record_index(ws, u64::try_from(index_ms).unwrap_or(u64::MAX));
    }
    let report = status_of(ws, note);
    if json {
        (erased::Json::to_json(&report), 0)
    } else {
        (render_status(&report), 0)
    }
}

/// Arguments of `wn ask` (without the context, which is read once by the caller).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct AskArgs {
    pub query: String,
    pub functions: bool,
    pub k: usize,
    pub no_adapter: bool,
    pub strict: bool,
    pub no_abstain: bool,
    pub start: bool,
    pub start_min_files: usize,
    pub no_log: bool,
}

/// `wn ask` on an opened workspace: text or JSON, and the exit code. Shared by the in-process
/// path and the daemon so both print the same thing.
pub fn ask_command(ws: &mut Workspace, args: &AskArgs, context: &str, json: bool) -> (String, i32) {
    ask_command_with(ws, args, context, json, true)
}

/// [`ask_command`]; `rescan: false` answers from the current index without rescanning the
/// repository (the daemon rescans at most every few hundred milliseconds).
pub fn ask_command_with(
    ws: &mut Workspace,
    args: &AskArgs,
    context: &str,
    json: bool,
    rescan: bool,
) -> (String, i32) {
    use wn_core::rank::AnswerState;
    // The same check `suggest` makes (so the daemon and MCP agree), before touching the index.
    if let Some(outcome) = wn_core::runtime::empty_query(&args.query, context) {
        let text = if json {
            erased::Json::to_json(&outcome)
        } else {
            format!("where-next: {}", wn_core::runtime::EMPTY_QUERY)
        };
        return (text, 2);
    }
    let started = std::time::Instant::now();
    let refresh = if rescan {
        ws.refresh(args.functions).map(|_| ())
    } else {
        Ok(())
    };
    // Check the adapter state first: `needs_fit` runs git, which an active adapter never needs.
    if refresh.is_ok()
        && !args.no_adapter
        && ws.adapter_life.state() != AdapterState::Active
        && ws.needs_fit()
    {
        let _ = ws.fit();
    }
    let opts = SuggestOptions {
        k: args.k.max(1),
        with_functions: args.functions,
        adapt: !args.no_adapter,
        strict_abstain: args.strict,
        no_abstain: args.no_abstain,
        unsupported_only: ws.coverage.unsupported > 0,
        start_min_files: args.start.then_some(args.start_min_files),
    };
    let adapter = if ws.adapter_life.state().applies() {
        ws.adapter.as_ref()
    } else {
        None
    };
    let outcome: Outcome = suggest_with_exact(
        &ws.index,
        adapter,
        ws.encoder.as_ref(),
        &args.query,
        context,
        opts,
        Some(&ws.root),
    );
    if !args.no_log {
        record_query(
            ws,
            &args.query,
            context,
            &outcome,
            started.elapsed().as_millis(),
        );
    }
    if json {
        return (erased::Json::to_json(&outcome), 0);
    }
    let note = match outcome.state {
        AnswerState::Ok | AnswerState::Abstain | AnswerState::StaleIndex if ws.info.fallback => {
            Some(match ws.info.reason.as_deref() {
                None | Some(NO_MODEL) => NO_MODEL_HINT.to_string(),
                Some(reason) => format!("{reason}; hints use the lexical fallback"),
            })
        }
        _ => None,
    };
    (ask_text::render(&outcome, note.as_deref()), 0)
}

fn run_bench(cli: Cli) -> (String, i32) {
    let Command::Bench {
        history: _,
        contextbench,
        commits,
        train,
        step,
        with_tests,
        no_adapter,
    } = cli.command
    else {
        unreachable!("called for bench only");
    };
    let ws = Workspace::open(&cli.path, cli.model.as_deref());
    let opts = bench::BenchOptions {
        commits: commits.max(1),
        train: train.max(1),
        step: step.max(1),
        with_tests,
        adapter: !no_adapter,
        ..bench::BenchOptions::default()
    };
    let model_name = ws
        .info
        .model
        .clone()
        .unwrap_or_else(|| "lexical fallback (no model installed)".into());
    let mut log = |m: &str| bench::stderr_log(m);
    let result = match &contextbench {
        Some(dir) => {
            let fingerprint = ws.info.fingerprint.clone();
            let cache = move |root: &Path| repo_dir(root, &fingerprint).join("bench");
            bench::contextbench(
                dir,
                ws.encoder.as_ref(),
                &model_name,
                &cache,
                &opts,
                &mut log,
            )
        }
        None => bench::history(
            &ws.root,
            ws.encoder.as_ref(),
            &model_name,
            Some(&ws.index),
            &ws.dir.join("bench"),
            &opts,
            &mut log,
        ),
    };
    match result {
        Ok(report) => {
            if contextbench.is_none() {
                record_bench(&ws, &report);
            }
            let text = if cli.json {
                serde_json::to_string_pretty(&report).unwrap_or_default()
            } else {
                bench::render(&report)
            };
            (text, 0)
        }
        Err(e) => (format!("wn bench: {e}"), 1),
    }
}

#[cfg(not(feature = "onnx"))]
fn serve_mcp(_cli: &Cli) -> (String, i32) {
    (
        "wn mcp: this build has no onnx feature; rebuild with default features".into(),
        2,
    )
}

/// `wn mcp`: serves the MCP tools over stdio until the client disconnects. Uses the same model
/// resolution and cache layout as the other commands, so `wn init` warms the server's index.
/// Diagnostics go to stderr; stdout carries MCP. Returns an empty text so nothing else prints.
#[cfg(feature = "onnx")]
fn serve_mcp(cli: &Cli) -> (String, i32) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return (format!("wn mcp: cannot start runtime: {e}"), 1),
    };
    // Clients often start MCP servers in `~` or another non-repository: serve anyway and fail
    // open on every call with the reason, rather than exiting or indexing that directory.
    let home_dir = std::env::var_os("HOME").map(PathBuf::from);
    if let Err(reason) = check_repo(&cli.path, cli.any_dir, home_dir.as_deref()) {
        let reason = format!(
            "{} For MCP: start the server in the project directory, or register it as \
             `wn --path <repo> mcp`.",
            reason.trim_start_matches("wn: ").replace('\n', " ")
        );
        eprintln!("where-next: {reason}");
        let service: Arc<std::sync::Mutex<dyn wn_daemon::daemon::Service>> =
            Arc::new(std::sync::Mutex::new(wn_daemon::daemon::Unavailable {
                reason,
            }));
        return match runtime.block_on(wn_mcp::serve_stdio(service)) {
            Ok(()) => (String::new(), 0),
            Err(e) => (format!("wn mcp: {e}"), 1),
        };
    }
    let root = project_root(&cli.path);
    // `open_repo` falls back to the lexical encoder when this path has no verified model.
    let model =
        resolve_model(cli.model.as_deref()).unwrap_or_else(|| models_home().join("none-installed"));
    let (service, refresher, choice) =
        wn_mcp::open_repo(&root, &model, &home(), std::time::Duration::from_secs(10));
    if let wn_mcp::EncoderChoice::LexicalFallback(reason) = &choice {
        eprintln!("where-next: model unavailable ({reason}); using the lexical fallback");
    }
    let result = runtime.block_on(wn_mcp::serve_stdio(service));
    refresher.stop();
    match result {
        Ok(()) => (String::new(), 0),
        Err(e) => (format!("wn mcp: {e}"), 1),
    }
}

mod erased {
    /// Object-safe JSON rendering for command reports.
    pub trait Json {
        fn to_json(&self) -> String;
    }

    impl<T: serde::Serialize> Json for T {
        fn to_json(&self) -> String {
            serde_json::to_string_pretty(self).unwrap_or_else(|e| format!("{{\"error\": \"{e}\"}}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_resolution_prefers_flag_then_env_then_best_installed() {
        let models = tempfile::tempdir().unwrap();
        let m = models.path();
        assert_eq!(resolve_model_in(None, None, m), None);
        std::fs::create_dir_all(m.join("v2b")).unwrap();
        std::fs::write(m.join("v2b/wn-model.json"), "{}").unwrap();
        assert_eq!(resolve_model_in(None, None, m), Some(m.join("v2b")));
        std::fs::create_dir_all(m.join("gemma-g2r")).unwrap();
        std::fs::write(m.join("gemma-g2r/wn-model.json"), "{}").unwrap();
        assert_eq!(resolve_model_in(None, None, m), Some(m.join("gemma-g2r")));
        std::fs::create_dir_all(m.join("gemma-xl1")).unwrap();
        std::fs::write(m.join("gemma-xl1/wn-model.json"), "{}").unwrap();
        assert_eq!(resolve_model_in(None, None, m), Some(m.join("gemma-xl1")));
        let env = Path::new("/env/model");
        assert_eq!(resolve_model_in(None, Some(env), m), Some(env.into()));
        let flag = Path::new("/flag/model");
        assert_eq!(
            resolve_model_in(Some(flag), Some(env), m),
            Some(flag.into())
        );
    }
}
