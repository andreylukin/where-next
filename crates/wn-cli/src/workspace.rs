//! Workspace search: `wn ask`, `wn status`, `wn init` and `wn mcp` in a directory that holds
//! repositories (such as `~`) instead of inside one.
//!
//! - The workspace is every repository at or below the directory that has an index for the model
//!   in use, found through the root each cache directory records. Nothing is indexed at query
//!   time: `wn init` in the directory discovers the git repositories below it and indexes each.
//! - A query is embedded once. Each repository ranks it with its own index, adapter and abstain
//!   thresholds, and repositories that abstain drop out (the repository tier). The remaining
//!   rankings are merged by reciprocal rank fusion over per-repository ranks, ties broken by
//!   similarity (the file tier): every matching repository's best hint comes before any
//!   repository's second, so the top hints span repositories and scores from different
//!   adapters are never compared except to order equal ranks.
//! - Paths are relative to the workspace directory, so they open from there as printed.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Instant;

use serde::Serialize;
use wn_core::encoder::{EncodeError, Encoder, QueryInput};
use wn_core::index::EntryKind;
use wn_core::rank::{AdapterUse, AnswerState, Calibration, Hint, Hints, Outcome};
use wn_daemon::daemon::{state_label, Reply, Service, Status};
use wn_daemon::workspace::{cached_repos, model_cache_dir, repo_cache_dir, Provenance, Refreshed};

use crate::ask_text::{escape_controls, paint, DIM, WARN};
use crate::{
    ask_outcome, count, erased::Json as _, AskArgs, Cli, Command, EncoderInfo, SharedEncoder,
    StatusKind, Workspace,
};

/// How deep `wn init` looks for repositories below the workspace directory
/// (`~/src/github.com/org/repo` is 4).
pub const MAX_DEPTH: usize = 4;
/// `wn init` asks before indexing more repositories than this (`--yes` skips the question).
pub const CONFIRM_ABOVE: usize = 10;
/// The reciprocal rank fusion constant.
const RRF_K: f64 = 60.0;
/// Directories never searched for repositories (besides hidden ones).
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "vendor",
    "dist",
    "build",
    "Library",
    "__pycache__",
];

/// Git repositories below `dir` (not `dir` itself), at most `max_depth` levels down. A directory
/// with a `.git` entry (a directory, or a file for worktrees and submodules) is a repository and
/// is not searched further; hidden directories, [`SKIP_DIRS`] and symbolic links are skipped.
pub fn discover(dir: &Path, max_depth: usize) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(dir.to_path_buf(), 0)];
    while let Some((d, depth)) = stack.pop() {
        if depth > 0 && d.join(".git").exists() {
            found.push(d);
            continue;
        }
        if depth == max_depth {
            continue;
        }
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            // `file_type` does not follow links, so a link (or a loop of them) is never entered.
            let dir = e.file_type().is_ok_and(|t| t.is_dir());
            if dir && !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                stack.push((e.path(), depth + 1));
            }
        }
    }
    found.sort();
    found
}

/// Repositories at or below `dir` whose cache holds an index built with the model
/// `fingerprint`.
pub fn indexed(cache_home: &Path, dir: &Path, fingerprint: &str) -> Vec<PathBuf> {
    cached_repos(cache_home)
        .into_iter()
        .filter(|root| root.starts_with(dir))
        .filter(|root| {
            model_cache_dir(cache_home, root, fingerprint)
                .join("index")
                .exists()
        })
        .collect()
}

/// [`indexed`] by model name instead of fingerprint (no model load): each repository with its
/// index directory.
pub fn indexed_named(cache_home: &Path, dir: &Path, model: &str) -> Vec<(PathBuf, PathBuf)> {
    let prefix = format!("{model}-");
    cached_repos(cache_home)
        .into_iter()
        .filter(|root| root.starts_with(dir))
        .filter_map(|root| {
            let entries = std::fs::read_dir(repo_cache_dir(cache_home, &root)).ok()?;
            let index = entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .map(|e| e.path().join("index"))
                .find(|p| p.exists())?;
            Some((root, index))
        })
        .collect()
}

/// The model name `wn` would use and the prefix of its index directories (`hash` for the
/// lexical fallback).
fn model_label(explicit: Option<&Path>) -> (String, String) {
    match crate::hooks::model_name_for(explicit) {
        Some(name) => (name.clone(), name),
        None => ("lexical fallback".into(), "hash".into()),
    }
}

fn repositories(n: usize) -> String {
    format!("{n} {}", if n == 1 { "repository" } else { "repositories" })
}

/// `root` relative to the workspace `dir`, with `/` separators (empty for `dir` itself).
fn relative(dir: &Path, root: &Path) -> String {
    let rel = root.strip_prefix(dir).unwrap_or(root);
    rel.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// A repository's display name in a workspace: its relative directory, or its name when it is
/// the workspace directory itself.
fn repo_name(dir: &Path, root: &Path) -> String {
    let rel = relative(dir, root);
    if rel.is_empty() {
        root.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        rel
    }
}

// ---------------------------------------------------------------------------------------------
// Asking
// ---------------------------------------------------------------------------------------------

/// Queries already embedded, with their vectors.
type Embedded = Vec<(Vec<QueryInput>, Vec<Vec<f32>>)>;

/// Embeds each distinct query once, however many repositories rank it.
pub struct OnceEncoder<'a> {
    inner: &'a (dyn Encoder + Send + Sync),
    seen: Mutex<Embedded>,
}

impl<'a> OnceEncoder<'a> {
    pub fn new(inner: &'a (dyn Encoder + Send + Sync)) -> Self {
        Self {
            inner,
            seen: Mutex::default(),
        }
    }
}

impl Encoder for OnceEncoder<'_> {
    fn fingerprint(&self) -> String {
        self.inner.fingerprint()
    }

    fn calibration(&self) -> Option<Calibration> {
        self.inner.calibration()
    }

    fn documents(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
        self.inner.documents(texts)
    }

    fn queries(&self, items: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
        let mut seen = self.seen.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, v)) = seen.iter().find(|(q, _)| q == items) {
            return Ok(v.clone());
        }
        let v = self.inner.queries(items)?;
        seen.push((items.to_vec(), v.clone()));
        Ok(v)
    }
}

/// One repository's answer to a workspace query.
#[derive(Debug, Clone)]
pub struct RepoResult {
    pub root: PathBuf,
    /// Indexed source files.
    pub files: usize,
    pub outcome: Outcome,
}

/// Answers a workspace query in one repository from its current index (no rescan, no adapter
/// fit: `wn init` does both).
pub fn repo_result(
    ws: &mut Workspace,
    encoder: &dyn Encoder,
    args: &AskArgs,
    context: &str,
) -> RepoResult {
    RepoResult {
        root: ws.root.clone(),
        files: ws.index.count(EntryKind::File),
        outcome: ask_outcome(ws, encoder, args, context, false, false),
    }
}

/// A repository left out of a workspace answer, and why.
pub fn skipped(root: &Path, why: &str) -> RepoResult {
    RepoResult {
        root: root.to_path_buf(),
        files: 0,
        outcome: Outcome {
            state: AnswerState::Error,
            error: Some(why.into()),
            ..Outcome::default()
        },
    }
}

/// Which list of an answer a hint belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Part {
    File,
    Function,
    Config,
}

/// One hint in a workspace answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkspaceHint {
    /// The hint, its `path` relative to the workspace directory.
    #[serde(flatten)]
    pub hint: Hint,
    /// The repository, relative to the workspace directory.
    pub repo: String,
    /// The repository root (absolute).
    pub root: String,
    /// The path inside the repository.
    pub repo_path: String,
    /// 1-based rank inside its repository.
    pub rank: usize,
    /// Reciprocal rank fusion score.
    pub fused: f64,
    #[serde(skip)]
    part: Part,
}

/// One repository in a workspace answer.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RepoSummary {
    pub repo: String,
    pub root: String,
    /// Answer state (`ok`, `abstain`, `error`, …).
    pub state: &'static str,
    /// Its best hint's similarity, when it answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub top: Option<f64>,
    /// Hints of this repository in the answer.
    pub shown: usize,
    pub files: usize,
    pub adapter: bool,
    /// Why it abstained or failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A workspace answer. Its JSON reads as a `wn ask` answer (state, files, functions, configs)
/// plus the workspace and a row per repository.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Answer {
    pub state: AnswerState,
    pub fallback: bool,
    pub workspace: String,
    pub files: Vec<WorkspaceHint>,
    pub functions: Vec<WorkspaceHint>,
    pub configs: Vec<WorkspaceHint>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abstain: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Repositories that answered, best top hint first, then the others.
    pub repos: Vec<RepoSummary>,
}

/// Merges per-repository answers into the top `k` hints (see the module docs).
pub fn fuse(dir: &Path, results: &[RepoResult], k: usize, fallback: bool) -> Answer {
    let mut pool = Vec::new();
    for r in results
        .iter()
        .filter(|r| r.outcome.state == AnswerState::Ok)
    {
        let h = &r.outcome.hints;
        let ranked = (h.files.iter().map(|x| (Part::File, x)))
            .chain(h.functions.iter().map(|x| (Part::Function, x)))
            .chain(h.configs.iter().map(|x| (Part::Config, x)));
        let repo = repo_name(dir, &r.root);
        let prefix = relative(dir, &r.root);
        for (i, (part, hint)) in ranked.enumerate() {
            let mut shown = hint.clone();
            if !prefix.is_empty() {
                shown.path = format!("{prefix}/{}", hint.path);
            }
            pool.push(WorkspaceHint {
                hint: shown,
                repo: repo.clone(),
                root: r.root.to_string_lossy().into_owned(),
                repo_path: hint.path.clone(),
                rank: i + 1,
                fused: 1.0 / (RRF_K + (i + 1) as f64),
                part,
            });
        }
    }
    pool.sort_by(|a, b| {
        b.fused
            .total_cmp(&a.fused)
            .then(b.hint.similarity.total_cmp(&a.hint.similarity))
            .then_with(|| a.repo.cmp(&b.repo))
    });
    pool.truncate(k.max(1));

    let mut repos: Vec<RepoSummary> = results
        .iter()
        .map(|r| {
            let repo = repo_name(dir, &r.root);
            let o = &r.outcome;
            RepoSummary {
                shown: pool.iter().filter(|h| h.repo == repo).count(),
                top: (o.state == AnswerState::Ok)
                    .then(|| {
                        let h = &o.hints;
                        h.files.iter().chain(&h.functions).chain(&h.configs)
                    })
                    .and_then(|mut all| all.next().map(|h| h.similarity)),
                repo,
                root: r.root.to_string_lossy().into_owned(),
                state: state_label(o.state),
                files: r.files,
                adapter: o.adapter.applied,
                reason: o.abstain.clone().or_else(|| o.error.clone()),
            }
        })
        .collect();
    repos.sort_by(|a, b| {
        let top = |r: &RepoSummary| r.top.unwrap_or(f64::NEG_INFINITY);
        top(b).total_cmp(&top(a)).then_with(|| a.repo.cmp(&b.repo))
    });

    let (state, abstain, error) = if !pool.is_empty() {
        (AnswerState::Ok, None, None)
    } else if results.is_empty() {
        (
            AnswerState::Error,
            None,
            Some(format!(
                "no repository below {} is indexed with this model; run `wn init` there",
                dir.display()
            )),
        )
    } else if results
        .iter()
        .all(|r| r.outcome.state == AnswerState::Error)
    {
        let first = results[0].outcome.error.clone().unwrap_or_default();
        (AnswerState::Error, None, Some(first))
    } else {
        let why = format!("{} searched", repositories(results.len()));
        (AnswerState::Abstain, Some(why), None)
    };
    let part =
        |p: Part| -> Vec<WorkspaceHint> { pool.iter().filter(|h| h.part == p).cloned().collect() };
    Answer {
        state,
        fallback,
        workspace: dir.to_string_lossy().into_owned(),
        files: part(Part::File),
        functions: part(Part::Function),
        configs: part(Part::Config),
        abstain,
        error,
        repos,
    }
}

impl Answer {
    /// The answer as a plain `wn ask` outcome (paths relative to the workspace directory).
    pub fn to_outcome(&self) -> Outcome {
        let hints = |v: &[WorkspaceHint]| v.iter().map(|h| h.hint.clone()).collect();
        Outcome {
            state: self.state,
            fallback: self.fallback,
            hints: Hints {
                files: hints(&self.files),
                functions: hints(&self.functions),
                configs: hints(&self.configs),
            },
            abstain: self.abstain.clone(),
            error: self.error.clone(),
            adapter: AdapterUse {
                applied: self.repos.iter().any(|r| r.shown > 0 && r.adapter),
                ..AdapterUse::default()
            },
        }
    }

    /// The one-line repository tier under the hints: how many repositories were searched and
    /// which ones the hints came from.
    fn summary(&self) -> String {
        let from: Vec<String> = self
            .repos
            .iter()
            .filter(|r| r.shown > 0)
            .map(|r| escape_controls(&r.repo))
            .collect();
        let quiet = self.repos.iter().filter(|r| r.state == "abstain").count();
        let failed = self
            .repos
            .iter()
            .filter(|r| !matches!(r.state, "ok" | "abstain"))
            .count();
        let mut line = format!(
            "{} searched; hints from {}",
            repositories(self.repos.len()),
            from.join(", ")
        );
        if quiet > 0 {
            line.push_str(&format!("; no confident hint in {quiet}"));
        }
        if failed > 0 {
            line.push_str(&format!("; {failed} skipped (see --json)"));
        }
        line
    }
}

/// Text (styled) for a workspace answer: the hints, the repository line, then `note`.
pub fn render(a: &Answer, note: Option<&str>) -> String {
    let mut text = crate::ask_text::render(&a.to_outcome(), None);
    if a.state == AnswerState::Ok {
        text.push('\n');
        text.push_str(&paint(DIM, &a.summary()));
    }
    if let Some(note) = note {
        text.push_str(&format!(
            "\n{} {}",
            paint(WARN, "note:"),
            escape_controls(note)
        ));
    }
    text
}

/// Records the answer in the usage log of each repository it has hints from (as that
/// repository's own `wn ask` would), so `wn stats` can follow them.
fn record(
    cache_home: &Path,
    answer: &Answer,
    results: &[RepoResult],
    args: &AskArgs,
    context: &str,
    model: &str,
    ms: u128,
) {
    if args.no_log {
        return;
    }
    for r in results {
        let root = r.root.to_string_lossy();
        let hinted: Vec<String> = answer
            .files
            .iter()
            .filter(|h| h.root == root)
            .map(|h| h.repo_path.clone())
            .collect();
        if hinted.is_empty() {
            continue;
        }
        let event = wn_daemon::usage::QueryEvent {
            ts: wn_daemon::usage::now(),
            kind: wn_core::rank::QueryKind::classify(&args.query, context)
                .as_str()
                .to_string(),
            state: "ok".into(),
            ms: u64::try_from(ms).unwrap_or(u64::MAX),
            model: model.to_string(),
            adapter: r.outcome.adapter.applied,
            files: r.files,
            hinted,
        };
        let dir = repo_cache_dir(cache_home, &r.root);
        let _ = wn_daemon::usage::record_query(&dir, &r.root, &event);
    }
}

/// Fuses, logs and renders a workspace answer: text or JSON, and the exit code. Shared by the
/// in-process path and the daemon so both print the same thing.
pub fn respond(
    dir: &Path,
    results: &[RepoResult],
    info: &EncoderInfo,
    args: &AskArgs,
    context: &str,
    json: bool,
    started: Instant,
) -> (String, i32) {
    let answer = fuse(dir, results, args.k, info.fallback);
    record(
        &crate::home(),
        &answer,
        results,
        args,
        context,
        &info.fingerprint,
        started.elapsed().as_millis(),
    );
    if json {
        return (answer.to_json(), 0);
    }
    let note =
        (info.fallback && answer.state != AnswerState::Error).then(|| crate::fallback_note(info));
    (render(&answer, note.as_deref()), 0)
}

/// Answers a workspace query in this process.
fn ask_here(cli: &Cli, dir: &Path, args: &AskArgs, context: &str) -> (String, i32) {
    let render: std::sync::Arc<dyn crate::progress::Sink> =
        std::sync::Arc::new(crate::progress::Render::stderr());
    let loading = crate::progress::Ticker::step("loading the model", render);
    let (encoder, info) = crate::encoder(cli.model.as_deref());
    drop(loading);
    let started = Instant::now();
    let once = OnceEncoder::new(encoder.as_ref());
    let results: Vec<RepoResult> = indexed(&crate::home(), dir, &info.fingerprint)
        .iter()
        .map(|root| {
            let mut ws = Workspace::open_with(root, encoder.clone(), info.clone());
            repo_result(&mut ws, &once, args, context)
        })
        .collect();
    respond(dir, &results, &info, args, context, cli.json, started)
}

// ---------------------------------------------------------------------------------------------
// Status and init
// ---------------------------------------------------------------------------------------------

/// One indexed repository in `wn status`.
#[derive(Debug, Clone, Serialize)]
pub struct RepoStatus {
    pub repo: String,
    pub root: String,
    /// Indexed source files.
    pub files: usize,
    /// When the index was last written (Unix seconds).
    pub indexed_at: Option<u64>,
}

/// `wn status` in a workspace.
#[derive(Debug, Clone, Serialize)]
pub struct StatusReport {
    pub workspace: String,
    pub model: String,
    pub repos: Vec<RepoStatus>,
    /// Git repositories found below the directory without an index for the model.
    pub not_indexed: Vec<String>,
}

fn repo_status(dir: &Path, root: &Path, index: &Path) -> RepoStatus {
    let meta = index.join("meta.json");
    let files = std::fs::read(&meta)
        .ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .and_then(|v| {
            let entries = v.get("entries")?.as_array()?;
            Some(entries.iter().filter(|e| e["kind"] == "file").count())
        })
        .unwrap_or(0);
    let indexed_at = std::fs::metadata(&meta)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs());
    RepoStatus {
        repo: repo_name(dir, root),
        root: root.to_string_lossy().into_owned(),
        files,
        indexed_at,
    }
}

fn ago(then: Option<u64>, now: u64) -> String {
    let Some(then) = then else {
        return "not built yet".into();
    };
    let s = now.saturating_sub(then);
    let (n, unit) = match s {
        0..=59 => return "indexed just now".into(),
        60..=3599 => (s / 60, "minute"),
        3600..=86_399 => (s / 3600, "hour"),
        _ => (s / 86_400, "day"),
    };
    format!("indexed {} ago", count(n as usize, unit))
}

/// Text form of a workspace status.
pub fn render_status(s: &StatusReport, now: u64) -> String {
    let mut lines = vec![format!(
        "where-next workspace {}: {} of {} indexed (model: {})",
        escape_controls(&s.workspace),
        s.repos.len(),
        repositories(s.repos.len() + s.not_indexed.len()),
        escape_controls(&s.model)
    )];
    let rows: Vec<(String, String)> = s
        .repos
        .iter()
        .map(|r| (escape_controls(&r.repo), count(r.files, "source file")))
        .collect();
    let width = |col: fn(&(String, String)) -> &String| {
        rows.iter()
            .map(|r| col(r).chars().count())
            .max()
            .unwrap_or(0)
    };
    let (name_width, files_width) = (width(|r| &r.0), width(|r| &r.1));
    for ((name, files), r) in rows.iter().zip(&s.repos) {
        lines.push(format!(
            "  {name:name_width$}  {files:files_width$}  {}",
            ago(r.indexed_at, now)
        ));
    }
    if !s.not_indexed.is_empty() {
        let names: Vec<String> = s.not_indexed.iter().map(|n| escape_controls(n)).collect();
        lines.push(format!(
            "not indexed: {} (`wn init` here indexes them)",
            names.join(", ")
        ));
    }
    if s.repos.is_empty() {
        lines.push("`wn ask` here searches nothing until you run `wn init` here".into());
    } else {
        lines.push("`wn ask` here searches every indexed repository".into());
    }
    lines.join("\n")
}

fn status(cli: &Cli, dir: &Path, refused: String) -> (String, i32) {
    let (model, prefix) = model_label(cli.model.as_deref());
    let indexed = indexed_named(&crate::home(), dir, &prefix);
    let not_indexed: Vec<String> = discover(dir, MAX_DEPTH)
        .iter()
        .filter(|root| {
            let canon = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
            !indexed.iter().any(|(r, _)| *r == canon)
        })
        .map(|root| repo_name(dir, root))
        .collect();
    if indexed.is_empty() && not_indexed.is_empty() {
        return (refused, 2);
    }
    let report = StatusReport {
        workspace: dir.to_string_lossy().into_owned(),
        model,
        repos: indexed
            .iter()
            .map(|(root, index)| repo_status(dir, root, index))
            .collect(),
        not_indexed,
    };
    if cli.json {
        (report.to_json(), 0)
    } else {
        (render_status(&report, wn_daemon::usage::now()), 0)
    }
}

/// Whether to index `repos` from `dir`: yes when few, `--yes`, or confirmed on a terminal.
fn confirm(dir: &Path, repos: &[PathBuf], yes: bool) -> bool {
    use std::io::IsTerminal as _;
    if yes || repos.len() <= CONFIRM_ABOVE {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        return false;
    }
    eprint!(
        "index all {} repositories below {}? [y/N] ",
        repos.len(),
        dir.display()
    );
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes")
}

fn init(cli: &Cli, dir: &Path, refused: String, yes: bool) -> (String, i32) {
    let repos = discover(dir, MAX_DEPTH);
    if repos.is_empty() {
        return (refused, 2);
    }
    eprintln!(
        "wn: found {} below {}:",
        repositories(repos.len()),
        escape_controls(&dir.display().to_string())
    );
    for root in &repos {
        eprintln!("  {}", escape_controls(&repo_name(dir, root)));
    }
    if !confirm(dir, &repos, yes) {
        return (
            format!(
                "wn init: not indexing {} repositories without confirmation; pass --yes to \
                 index them all, or run `wn init` inside the ones you use",
                repos.len()
            ),
            2,
        );
    }
    let render: std::sync::Arc<dyn crate::progress::Sink> =
        std::sync::Arc::new(crate::progress::Render::stderr());
    let loading = crate::progress::Ticker::step("loading the model", render.clone());
    let (encoder, info) = crate::encoder(cli.model.as_deref());
    drop(loading);
    let mut reports = Vec::new();
    let mut lines = Vec::new();
    let mut failed = Vec::new();
    for (i, root) in repos.iter().enumerate() {
        let name = escape_controls(&repo_name(dir, root));
        eprintln!("wn: [{}/{}] {name}", i + 1, repos.len());
        let mut ws = Workspace::open_with(root, encoder.clone(), info.clone());
        ws.progress = Some(render.clone());
        match crate::status_report(&mut ws, StatusKind::Init) {
            Ok(r) => {
                lines.push(format!(
                    "{name}: {}, adapter {}",
                    count(r.files, "source file"),
                    r.adapter_state.to_lowercase()
                ));
                reports.push(r);
            }
            Err(e) => {
                lines.push(format!("{name}: could not index: {}", escape_controls(&e)));
                failed.push(serde_json::json!({ "repo": repo_name(dir, root), "error": e }));
            }
        }
    }
    let code = i32::from(!failed.is_empty());
    if cli.json {
        let v = serde_json::json!({
            "workspace": dir,
            "repos": reports,
            "failed": failed,
            "encoder": info,
        });
        return (v.to_json(), code);
    }
    lines.push(format!(
        "indexed {} of {} below {}; `wn ask` there searches all of them",
        reports.len(),
        repositories(repos.len()),
        escape_controls(&dir.display().to_string())
    ));
    if info.fallback {
        lines.push(format!(
            "model: lexical fallback ({})",
            crate::fallback_why(info.reason.as_deref())
        ));
    }
    (lines.join("\n"), code)
}

/// `wn init | status | ask` where `check_repo` refused `cli.path` (with `refused`): run across
/// the repositories below it, or print `refused` when there are none.
pub fn run(cli: &Cli, refused: String) -> (String, i32) {
    let dir = cli.path.canonicalize().unwrap_or_else(|_| cli.path.clone());
    match &cli.command {
        Command::Init { yes } => init(cli, &dir, refused, *yes),
        Command::Status => status(cli, &dir, refused),
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
            no_log,
        } => {
            let (_, prefix) = model_label(cli.model.as_deref());
            if indexed_named(&crate::home(), &dir, &prefix).is_empty() {
                let found = discover(&dir, MAX_DEPTH).len();
                if found == 0 {
                    return (refused, 2);
                }
                return (
                    format!(
                        "{refused}\n`wn init` here indexes the {} below it; `wn ask` here \
                         then searches all of them.",
                        repositories(found)
                    ),
                    2,
                );
            }
            let context = crate::read_context(context_file);
            if let Some(outcome) = wn_core::runtime::empty_query(query, &context) {
                let text = if cli.json {
                    outcome.to_json()
                } else {
                    format!("where-next: {}", wn_core::runtime::EMPTY_QUERY)
                };
                return (text, 2);
            }
            if let Some(out) =
                crate::daemon::client::call(cli, crate::daemon::OpKind::Ask, &context)
            {
                return out;
            }
            let args = AskArgs {
                query: query.clone(),
                functions: *functions,
                k: *k,
                no_adapter: *no_adapter,
                strict: *strict,
                no_abstain: *no_abstain,
                start: *start,
                start_min_files: *start_min_files,
                no_log: *no_log,
            };
            ask_here(cli, &dir, &args, &context)
        }
        _ => (refused, 2),
    }
}

// ---------------------------------------------------------------------------------------------
// MCP
// ---------------------------------------------------------------------------------------------

/// `wn mcp` started in a workspace directory: every call searches the repositories indexed below
/// it (found again on each call, so a later `wn init` counts). The model loads on first use.
pub struct WorkspaceService {
    dir: PathBuf,
    model: Option<PathBuf>,
    /// Why this is not a single repository (shown when nothing below it is indexed).
    refused: String,
    encoder: OnceLock<(SharedEncoder, EncoderInfo)>,
    open: Mutex<Vec<Workspace>>,
}

impl WorkspaceService {
    pub fn new(dir: &Path, model: Option<&Path>, refused: String) -> Self {
        Self {
            dir: dir.to_path_buf(),
            model: model.map(Path::to_path_buf),
            refused,
            encoder: OnceLock::new(),
            open: Mutex::default(),
        }
    }

    fn encoder(&self) -> &(SharedEncoder, EncoderInfo) {
        self.encoder
            .get_or_init(|| crate::encoder(self.model.as_deref()))
    }

    fn none_indexed(&self) -> String {
        format!(
            "{} Run `wn init` in {} to index the git repositories below it for workspace search.",
            self.refused.trim_start_matches("wn: ").replace('\n', " "),
            self.dir.display()
        )
    }

    /// Runs `f` on the workspace of each indexed repository (opened once, then kept).
    fn each<T>(&self, mut f: impl FnMut(&mut Workspace) -> T) -> Vec<T> {
        let (encoder, info) = self.encoder();
        let roots = indexed(&crate::home(), &self.dir, &info.fingerprint);
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        open.retain(|ws| roots.contains(&ws.root));
        roots
            .iter()
            .map(|root| {
                let i = match open.iter().position(|ws| &ws.root == root) {
                    Some(i) => i,
                    None => {
                        open.push(Workspace::open_with(root, encoder.clone(), info.clone()));
                        open.len() - 1
                    }
                };
                f(&mut open[i])
            })
            .collect()
    }

    fn provenance(&self, files: usize) -> Provenance {
        Provenance {
            model: self.encoder().1.fingerprint.clone(),
            index_state: "Ready".into(),
            files_indexed: files,
            configs_indexed: 0,
            coverage: wn_git::Coverage::default(),
        }
    }
}

impl Service for WorkspaceService {
    fn ask(&self, query: &str, context: &str) -> Reply {
        self.ask_as(query, context, false)
    }

    fn ask_as(&self, query: &str, context: &str, task_start: bool) -> Reply {
        let started = Instant::now();
        let args = AskArgs {
            query: query.into(),
            functions: false,
            k: wn_core::rank::MAX_HINTS,
            no_adapter: false,
            strict: false,
            no_abstain: false,
            start: task_start,
            start_min_files: wn_core::rank::START_HINT_MIN_FILES,
            no_log: false,
        };
        let (encoder, info) = self.encoder();
        let once = OnceEncoder::new(encoder.as_ref());
        let (outcome, results) = match wn_core::runtime::empty_query(query, context) {
            Some(empty) => (empty, Vec::new()),
            None => {
                let results = self.each(|ws| repo_result(ws, &once, &args, context));
                let answer = fuse(&self.dir, &results, args.k, info.fallback);
                let ms = started.elapsed().as_millis();
                let fp = &info.fingerprint;
                record(&crate::home(), &answer, &results, &args, context, fp, ms);
                let mut outcome = answer.to_outcome();
                if results.is_empty() {
                    outcome.error = Some(self.none_indexed());
                }
                (outcome, results)
            }
        };
        Reply {
            outcome,
            session: "Serving".into(),
            provenance: self.provenance(results.iter().map(|r| r.files).sum()),
            ms: started.elapsed().as_millis(),
        }
    }

    fn refresh(&mut self) -> Result<Refreshed, String> {
        let started = Instant::now();
        let stats = self.each(|ws| {
            ws.refresh(false)
                .map(|s| (s, ws.index.count(EntryKind::File)))
        });
        if stats.is_empty() {
            return Err(self.none_indexed());
        }
        let mut total = Refreshed {
            encoded: 0,
            removed: 0,
            files_indexed: 0,
            adapter: None,
            ms: 0,
        };
        for s in stats {
            let (s, files) = s?;
            total.encoded += s.encoded;
            total.removed += s.removed;
            total.files_indexed += files;
        }
        total.ms = started.elapsed().as_millis();
        Ok(total)
    }

    fn status(&self) -> Status {
        let files: Vec<usize> = self.each(|ws| ws.index.count(EntryKind::File));
        Status {
            session: "Serving".into(),
            last_error: files.is_empty().then(|| self.none_indexed()),
            provenance: self.provenance(files.iter().sum()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(path: &str, similarity: f64) -> Hint {
        Hint {
            path: path.into(),
            similarity,
            evidence: None,
            name: None,
            line: None,
        }
    }

    fn result(root: &str, state: AnswerState, files: &[(&str, f64)]) -> RepoResult {
        RepoResult {
            root: PathBuf::from(root),
            files: 10,
            outcome: Outcome {
                state,
                hints: Hints {
                    files: files.iter().map(|(p, s)| hint(p, *s)).collect(),
                    ..Hints::default()
                },
                abstain: (state == AnswerState::Abstain).then(|| "request: weak".into()),
                ..Outcome::default()
            },
        }
    }

    #[test]
    fn fusion_takes_each_repository_best_hint_first_ordered_by_score() {
        let dir = Path::new("/home/me");
        let results = [
            result(
                "/home/me/repos/big",
                AnswerState::Ok,
                &[("a.rs", 0.9), ("b.rs", 0.88), ("c.rs", 0.87)],
            ),
            result("/home/me/repos/small", AnswerState::Ok, &[("x.rs", 0.4)]),
            result(
                "/home/me/src/mid",
                AnswerState::Ok,
                &[("m.rs", 0.6), ("n.rs", 0.5)],
            ),
        ];
        let a = fuse(dir, &results, 3, false);
        let paths: Vec<&str> = a.files.iter().map(|h| h.hint.path.as_str()).collect();
        // Every repository's first hint beats any second one, however high its score.
        assert_eq!(
            paths,
            ["repos/big/a.rs", "src/mid/m.rs", "repos/small/x.rs"]
        );
        assert_eq!(a.files[1].repo, "src/mid");
        assert_eq!(a.files[1].repo_path, "m.rs");
        assert_eq!(a.files[1].rank, 1);
        assert_eq!(a.state, AnswerState::Ok);
        // The repository tier: best top hint first.
        let order: Vec<&str> = a.repos.iter().map(|r| r.repo.as_str()).collect();
        assert_eq!(order, ["repos/big", "src/mid", "repos/small"]);
    }

    #[test]
    fn a_single_matching_repository_fills_the_answer_and_others_stay_capped() {
        let dir = Path::new("/w");
        let only = [result(
            "/w/a",
            AnswerState::Ok,
            &[("1.rs", 0.9), ("2.rs", 0.8), ("3.rs", 0.7)],
        )];
        assert_eq!(fuse(dir, &only, 3, false).files.len(), 3);
        // Two repositories: at most two hints from either in a top 3.
        let two = [
            result(
                "/w/a",
                AnswerState::Ok,
                &[("1.rs", 0.9), ("2.rs", 0.8), ("3.rs", 0.7)],
            ),
            result("/w/b", AnswerState::Ok, &[("1.rs", 0.2), ("2.rs", 0.1)]),
        ];
        let a = fuse(dir, &two, 3, false);
        let from_a = a.files.iter().filter(|h| h.repo == "a").count();
        assert_eq!((from_a, a.files.len()), (2, 3));
    }

    #[test]
    fn abstaining_repositories_drop_out() {
        let dir = Path::new("/w");
        let results = [
            result("/w/a", AnswerState::Abstain, &[]),
            result("/w/b", AnswerState::Ok, &[("hit.rs", 0.5)]),
            skipped(Path::new("/w/c"), "busy"),
        ];
        let a = fuse(dir, &results, 3, false);
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.files[0].hint.path, "b/hit.rs");
        let text = crate::ask_text::finish(render(&a, None), false);
        assert_eq!(
            text,
            "b/hit.rs  0.50\n3 repositories searched; hints from b; no confident hint in 1; \
             1 skipped (see --json)"
        );
        // Nobody confident: a workspace abstain.
        let none = fuse(dir, &results[..1], 3, false);
        assert_eq!(none.state, AnswerState::Abstain);
        assert!(none.files.is_empty());
        // Nothing indexed: an error that says what to run.
        let empty = fuse(dir, &[], 3, false);
        assert_eq!(empty.state, AnswerState::Error);
        assert!(empty.error.unwrap().contains("wn init"));
    }

    #[test]
    fn workspace_json_reads_as_a_plain_answer() {
        let dir = Path::new("/w");
        let a = fuse(
            dir,
            &[result("/w/a", AnswerState::Ok, &[("src/x.rs", 0.5)])],
            3,
            false,
        );
        let json = serde_json::to_string(&a).unwrap();
        let plain: Outcome = serde_json::from_str(&json).unwrap();
        assert_eq!(plain.state, AnswerState::Ok);
        assert_eq!(plain.hints.files[0].path, "a/src/x.rs");
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["files"][0]["root"], "/w/a");
        assert_eq!(v["files"][0]["repo_path"], "src/x.rs");
        assert_eq!(v["repos"][0]["state"], "ok");
    }

    fn git_init(dir: &Path) {
        std::fs::create_dir_all(dir.join(".git")).unwrap();
    }

    #[test]
    fn discovery_finds_repositories_and_stops_at_each() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        git_init(&d.join("repos/a"));
        git_init(&d.join("repos/a/nested")); // inside a repository: not searched
        std::fs::create_dir_all(d.join("repos/wt")).unwrap();
        std::fs::write(d.join("repos/wt/.git"), "gitdir: /elsewhere\n").unwrap(); // a worktree
        git_init(&d.join("src/github.com/org/deep"));
        git_init(&d.join("src/github.com/org/deeper/still/x")); // below the depth limit
        git_init(&d.join(".hidden/r"));
        git_init(&d.join("node_modules/pkg"));
        git_init(d); // the workspace itself is not one of its repositories
        let found: Vec<String> = discover(d, MAX_DEPTH)
            .iter()
            .map(|p| relative(d, p))
            .collect();
        assert_eq!(found, ["repos/a", "repos/wt", "src/github.com/org/deep"]);
    }

    #[cfg(unix)]
    #[test]
    fn discovery_never_follows_links() {
        let t = tempfile::tempdir().unwrap();
        let d = t.path();
        git_init(&d.join("real/r"));
        std::os::unix::fs::symlink(d, d.join("real/loop")).unwrap();
        std::os::unix::fs::symlink(d.join("real/r"), d.join("alias")).unwrap();
        let found: Vec<String> = discover(d, 50).iter().map(|p| relative(d, p)).collect();
        assert_eq!(found, ["real/r"]);
    }

    #[test]
    fn a_query_is_embedded_once_across_repositories() {
        struct Counting(std::sync::atomic::AtomicUsize);
        impl Encoder for Counting {
            fn fingerprint(&self) -> String {
                "c".into()
            }
            fn documents(&self, t: &[String]) -> Result<Vec<Vec<f32>>, EncodeError> {
                Ok(t.iter().map(|_| vec![1.0]).collect())
            }
            fn queries(&self, q: &[QueryInput]) -> Result<Vec<Vec<f32>>, EncodeError> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(q.iter().map(|_| vec![1.0]).collect())
            }
        }
        let inner = Counting(std::sync::atomic::AtomicUsize::new(0));
        let once = OnceEncoder::new(&inner);
        for _ in 0..5 {
            once.queries(&[QueryInput::file("where is x")]).unwrap();
        }
        std::thread::scope(|scope| {
            for _ in 0..5 {
                scope.spawn(|| {
                    once.queries(&[QueryInput::file("where is x")]).unwrap();
                });
            }
        });
        once.queries(&[QueryInput::file("other")]).unwrap();
        assert_eq!(inner.0.load(std::sync::atomic::Ordering::Relaxed), 2);
    }
}
