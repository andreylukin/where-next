//! The `wn` command: index a repository, fit its personal adapter, and answer "where next?".
//!
//! Commands are thin wrappers over reports defined here so they can be tested without spawning
//! a process. Every report has a JSON form (`--json`) for agents and a short text form for people.

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
    },
    /// Show the index, coverage and adapter state.
    Status,
    /// Refit the personal adapter now.
    Train,
    /// Restore the previous adapter (or remove the only one).
    Rollback,
    /// Serve hints over MCP (stdio).
    Mcp,
}

/// Cache root: `$WHERE_NEXT_HOME`, else `~/.cache/where-next`.
pub fn home() -> PathBuf {
    if let Some(h) = std::env::var_os("WHERE_NEXT_HOME") {
        return PathBuf::from(h);
    }
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(".cache").join("where-next")
}

/// Per-repository, per-model cache directory.
pub fn repo_dir(root: &Path, fingerprint: &str) -> PathBuf {
    let rid = sha1_smol::Sha1::from(root.to_string_lossy().as_bytes())
        .digest()
        .to_string();
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let tag: String = fingerprint
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    home().join(format!("{name}-{}", &rid[..12])).join(tag)
}

/// Which encoder is in use.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct EncoderInfo {
    /// Fingerprint of the encoder (vector cache key).
    pub fingerprint: String,
    /// True when no model is installed and the lexical fallback is used.
    pub fallback: bool,
}

/// The encoder to use. Until a model backend is installed this is the lexical fallback.
pub fn encoder() -> (Box<dyn Encoder>, EncoderInfo) {
    let e = HashEncoder::default();
    let info = EncoderInfo {
        fingerprint: e.fingerprint(),
        fallback: true,
    };
    (Box::new(e), info)
}

/// A repository opened for one command.
pub struct Workspace {
    /// Repository root.
    pub root: PathBuf,
    /// Cache directory for this repository and model.
    pub dir: PathBuf,
    /// Encoder.
    pub encoder: Box<dyn Encoder>,
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
    /// Opens the repository containing `path`.
    pub fn open(path: &Path) -> Workspace {
        let root = repo_root(path);
        let (encoder, info) = encoder();
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
    lines.push(format!(
        "model: {}{}",
        s.encoder.fingerprint,
        if s.encoder.fallback {
            " (lexical fallback: no model installed)"
        } else {
            ""
        }
    ));
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
    let mut ws = Workspace::open(&cli.path);
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
        Command::Mcp => (
            "wn mcp: the MCP server lands with the wn-mcp crate; use `wn ask --json` meanwhile."
                .to_string(),
            2,
        ),
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
