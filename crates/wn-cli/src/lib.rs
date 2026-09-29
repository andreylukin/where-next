//! The `wn` command: index a repository, fit its personal adapter, and answer "where next?".
//!
//! Commands are thin wrappers over reports defined here so they can be tested without spawning
//! a process. Every report has a JSON form (`--json`) for agents and a short text form for people.

pub mod bench;

use std::io::Read as _;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use serde::Serialize;
use wn_core::adapter::{AdapterParams, ADAPTER_COMMITS, ADAPTER_REFIT_EVERY};
use wn_core::adapter_lifecycle::{AdapterEvent, AdapterLifecycle, AdapterState};
use wn_core::encoder::{Encoder, HashEncoder};
use wn_core::index::{EntryKind, Index, IndexedFile, RefreshStats};
use wn_core::index_lifecycle::IndexState;
use wn_core::rank::{render, Outcome};
use wn_core::runtime::{
    fit_from_history, load_adapter, save_adapter, suggest, HistoryExample, StoredAdapter,
    SuggestOptions,
};
use wn_git::{commits_since, history, repo_root, scan, Coverage};
use wn_sources::{read_text, Kind, MAX_CONFIG_BYTES, MAX_SOURCE_BYTES};

/// Command-line interface.
#[derive(Debug, Parser)]
#[command(
    name = "wn",
    version,
    about = "Fast local \"where next\" hints for coding agents and developers"
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
    /// What to do.
    #[command(subcommand)]
    pub command: Command,
}

/// Subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Index the repository and fit its personal adapter from git history.
    Init,
    /// Rank the files (and optionally functions) to open next for a task.
    Ask {
        /// The task or request.
        query: String,
        /// File with recent context (conversation, last tool output); `-` reads stdin.
        #[arg(long)]
        context_file: Option<PathBuf>,
        /// Also rank functions.
        #[arg(long)]
        functions: bool,
        /// Maximum file hints.
        #[arg(short, default_value_t = 3)]
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
    },
    /// Show the index, coverage and adapter state.
    Status,
    /// Refit the personal adapter now.
    Train,
    /// Restore the previous adapter (or remove the only one).
    Rollback,
    /// Serve hints over MCP (stdio).
    Mcp,
    /// Measure hit@k on this repository's own history (default) or on ContextBench.
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

/// The encoder to use: the verified model from [`resolve_model`], or the lexical fallback
/// when no model is installed or it fails verification (hints still work, and say so).
pub fn encoder(model: Option<&Path>) -> (Box<dyn Encoder + Send + Sync>, EncoderInfo) {
    let fallback = |reason: String| {
        let e = HashEncoder::default();
        let info = EncoderInfo {
            fingerprint: e.fingerprint(),
            fallback: true,
            model: None,
            reason: Some(reason),
        };
        (Box::new(e) as Box<dyn Encoder + Send + Sync>, info)
    };
    let Some(dir) = resolve_model(model) else {
        return fallback("no model installed".into());
    };
    open_model(&dir).unwrap_or_else(|e| fallback(format!("model unavailable: {e}")))
}

#[cfg(feature = "onnx")]
fn open_model(dir: &Path) -> Result<(Box<dyn Encoder + Send + Sync>, EncoderInfo), String> {
    let e = wn_embed::core_encoder::OnnxEncoder::open(dir, None)?;
    let info = EncoderInfo {
        fingerprint: e.fingerprint(),
        fallback: false,
        model: Some(e.spec().name.clone()),
        reason: None,
    };
    Ok((Box::new(e), info))
}

#[cfg(not(feature = "onnx"))]
fn open_model(_dir: &Path) -> Result<(Box<dyn Encoder + Send + Sync>, EncoderInfo), String> {
    Err("built without the onnx feature".into())
}

/// A repository opened for one command.
pub struct Workspace {
    /// Repository root.
    pub root: PathBuf,
    /// Cache directory for this repository and model.
    pub dir: PathBuf,
    /// Encoder.
    pub encoder: Box<dyn Encoder + Send + Sync>,
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
}

impl Workspace {
    /// Opens the repository containing `path` with the model from [`resolve_model`].
    pub fn open(path: &Path, model: Option<&Path>) -> Workspace {
        let root = repo_root(path);
        let (encoder, info) = encoder(model);
        let dir = repo_dir(&root, &info.fingerprint);
        let index = Index::open(&dir.join("index"), &info.fingerprint);
        let adapter = load_adapter(&dir.join("adapter"));
        let adapter_life = AdapterLifecycle::starting_in(match &adapter {
            Some(a) if a.meta.base == info.fingerprint => AdapterState::Active,
            Some(_) => AdapterState::Invalidated,
            None => AdapterState::None,
        });
        Workspace {
            root,
            dir,
            encoder,
            info,
            index,
            adapter_life,
            adapter,
            coverage: Coverage::default(),
        }
    }

    /// Scans the repository and brings the index up to date.
    pub fn refresh(&mut self, with_functions: bool) -> Result<RefreshStats, String> {
        let (files, coverage) = scan(&self.root);
        self.coverage = coverage;
        let list: Vec<IndexedFile> = files
            .into_iter()
            .map(|(path, f)| IndexedFile {
                path,
                cid: f.id.as_str().to_string(),
                kind: f.kind,
            })
            .collect();
        let root = self.root.clone();
        let read = move |p: &str, kind: Kind| {
            let max = if kind == Kind::Source {
                MAX_SOURCE_BYTES
            } else {
                MAX_CONFIG_BYTES
            };
            read_text(&root.join(p), max).ok()
        };
        self.index
            .refresh(&list, &read, self.encoder.as_ref(), with_functions)
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

    /// Fits the adapter from history, keeping the previous one for `rollback`.
    pub fn fit(&mut self) -> Result<String, String> {
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
        "where-next: {} source files, {} config files{} indexed in {name} ({})",
        s.files,
        s.configs,
        if s.functions > 0 {
            format!(", {} functions", s.functions)
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
            "not indexed: {} files ({})",
            s.coverage.unsupported,
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
            "model: {} (lexical fallback: {})",
            s.encoder.fingerprint,
            reason.as_deref().unwrap_or("no model installed")
        ),
    });
    lines.join("\n")
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
    if let Command::Mcp = cli.command {
        return serve_mcp(&cli);
    }
    if let Command::Bench { .. } = cli.command {
        return run_bench(cli);
    }
    let mut ws = Workspace::open(&cli.path, cli.model.as_deref());
    let json = cli.json;
    let out = |value: &dyn erased::Json, text: String| if json { value.to_json() } else { text };
    match cli.command {
        Command::Init | Command::Status | Command::Train => {
            let mut note = None;
            if let Err(e) = ws.refresh(false) {
                note = Some(format!("index failed: {e}"));
            }
            let should_fit = match cli.command {
                Command::Train => true,
                Command::Init => ws.needs_fit(),
                _ => false,
            };
            if should_fit && ws.index.state() == IndexState::Ready {
                note = Some(ws.fit().unwrap_or_else(|e| e));
            }
            let report = status_of(&ws, note);
            let text = render_status(&report);
            (out(&report, text), 0)
        }
        Command::Ask {
            query,
            context_file,
            functions,
            k,
            no_adapter,
            strict,
            no_abstain,
            start,
            start_min_files,
        } => {
            let context = read_context(&context_file);
            let refresh = ws.refresh(functions);
            if refresh.is_ok()
                && !no_adapter
                && ws.needs_fit()
                && ws.adapter_life.state() != AdapterState::Active
            {
                let _ = ws.fit();
            }
            let opts = SuggestOptions {
                k: k.max(1),
                with_functions: functions,
                adapt: !no_adapter,
                strict_abstain: strict,
                no_abstain,
                unsupported_only: ws.coverage.unsupported > 0,
                start_min_files: start.then_some(start_min_files),
            };
            let adapter = if ws.adapter_life.state().applies() {
                ws.adapter.as_ref()
            } else {
                None
            };
            let outcome: Outcome = suggest(
                &ws.index,
                adapter,
                ws.encoder.as_ref(),
                &query,
                &context,
                opts,
            );
            let text = render(&outcome);
            (out(&outcome, text), 0)
        }
        Command::Rollback => {
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
                "adapter: nothing to roll back".to_string()
            };
            let value = serde_json::json!({ "message": msg });
            (out(&value, msg.clone()), 0)
        }
        Command::Mcp | Command::Bench { .. } => unreachable!("handled above"),
    }
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
    let root = repo_root(&cli.path);
    // `open_repo` falls back to the lexical encoder when this path has no verified model.
    let model =
        resolve_model(cli.model.as_deref()).unwrap_or_else(|| models_home().join("none-installed"));
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(r) => r,
        Err(e) => return (format!("wn mcp: cannot start runtime: {e}"), 1),
    };
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
