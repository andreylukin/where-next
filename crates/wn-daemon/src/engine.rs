//! The where-next engine for one repository: scan files, embed skeletons, keep the index fresh,
//! and answer queries with at most three hints.
//!
//! [`Encoder`] is the seam between the engine and the model: production uses ONNX (see
//! [`crate::onnx`]), tests use a deterministic fake so the whole pipeline runs without weights.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde::Serialize;
use wn_core::index_lifecycle::{IndexEvent, IndexLifecycle, IndexState};

use crate::index::{Entry, FileKey, Index};

/// Maximum hints per answer (product principle: at most 3 paths).
pub const MAX_HINTS: usize = 3;
/// Largest file read for its skeleton.
pub const MAX_FILE_BYTES: usize = 400_000;

/// Turns texts into unit vectors. Queries and documents may be formatted differently.
pub trait Encoder {
    /// Output dimension.
    fn dim(&self) -> usize;
    /// Identity of the model (weights, tokenizer, document builder): keys the index snapshot.
    fn fingerprint(&self) -> String;
    /// Human-readable model name for provenance.
    fn name(&self) -> String;
    fn encode_documents(&mut self, docs: &[String]) -> Result<Vec<Vec<f32>>, String>;
    fn encode_query(&mut self, query: &str, context: &str) -> Result<Vec<f32>, String>;
}

/// One hint: a path, its similarity score and a short reason.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hint {
    pub path: String,
    /// Cosine similarity between query and document. Not a probability.
    pub similarity: f32,
    pub reason: String,
}

/// Where an answer came from, so callers can judge and reproduce it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provenance {
    pub model: String,
    pub model_fingerprint: String,
    pub index_state: String,
    pub documents: usize,
    pub adapter_applied: bool,
    pub ranking: &'static str,
}

/// Errors the engine reports; callers fail open on any of them.
#[derive(Debug)]
pub enum EngineError {
    Git(String),
    Encode(String),
    Index(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            EngineError::Git(m) => write!(f, "git: {m}"),
            EngineError::Encode(m) => write!(f, "encode: {m}"),
            EngineError::Index(m) => write!(f, "index: {m}"),
        }
    }
}

impl std::error::Error for EngineError {}

/// Tracked and untracked (not ignored) files, relative to `root`, with `/` separators.
pub fn list_files(root: &Path) -> Result<Vec<String>, EngineError> {
    let run = |args: &[&str]| -> Result<Vec<String>, EngineError> {
        let out = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .map_err(|e| EngineError::Git(e.to_string()))?;
        if !out.status.success() {
            return Err(EngineError::Git(
                String::from_utf8_lossy(&out.stderr).trim().to_string(),
            ));
        }
        Ok(out
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .collect())
    };
    let mut files = run(&["ls-files", "-z"])?;
    files.extend(run(&["ls-files", "--others", "--exclude-standard", "-z"])?);
    let mut seen = HashSet::new();
    files.retain(|f| seen.insert(f.clone()));
    // Tracked files deleted in the working tree are still listed by `git ls-files`.
    files.retain(|f| root.join(f).is_file());
    Ok(files)
}

/// Whether a path gets a code skeleton document.
pub fn indexable(path: &str) -> bool {
    wn_sources::is_source(path) && !wn_sources::is_skipped(path)
}

/// Words of the query (lowercase, 3+ characters) that appear in the document's names line; the
/// hint reason. Falls back to the document's first doc line.
pub fn reason(query: &str, doc: &str) -> String {
    let words: HashSet<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 3)
        .map(str::to_lowercase)
        .collect();
    let lines: Vec<&str> = doc.lines().collect();
    let names = lines.last().copied().unwrap_or("");
    let matched: Vec<&str> = names
        .split_whitespace()
        .filter(|n| {
            let low = n.to_lowercase();
            words.iter().any(|w| low.contains(w.as_str()))
        })
        .take(4)
        .collect();
    if !matched.is_empty() {
        return format!("defines {}", matched.join(", "));
    }
    match lines.get(1) {
        Some(doc_line) if lines.len() > 2 => format!("about: {doc_line}"),
        _ => "similar to the task description".to_string(),
    }
}

/// Files to (re-)embed and the set of files that currently exist; computed without touching the
/// index so it can run outside any lock.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Plan {
    pub current: HashSet<String>,
    pub todo: Vec<(String, FileKey)>,
    pub deleted: usize,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.todo.is_empty() && self.deleted == 0
    }
}

/// Lists indexable files and compares their change keys with what is indexed.
pub fn plan_changes(root: &Path, known: &HashMap<String, FileKey>) -> Result<Plan, EngineError> {
    let current: HashSet<String> = list_files(root)?
        .into_iter()
        .filter(|f| indexable(f))
        .collect();
    let mut todo: Vec<(String, FileKey)> = Vec::new();
    let mut sorted: Vec<&String> = current.iter().collect();
    sorted.sort();
    for f in sorted {
        let Ok(key) = FileKey::of(&root.join(f)) else {
            continue;
        };
        if known.get(f.as_str()) != Some(&key) {
            todo.push((f.clone(), key));
        }
    }
    let deleted = known.keys().filter(|k| !current.contains(*k)).count();
    Ok(Plan {
        current,
        todo,
        deleted,
    })
}

/// The engine for one repository.
pub struct Engine<E: Encoder> {
    root: PathBuf,
    cache_dir: Option<PathBuf>,
    encoder: E,
    index: Index,
    lifecycle: IndexLifecycle,
}

/// Summary of an index build or refresh.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RefreshStats {
    pub embedded: usize,
    pub removed: usize,
    pub documents: usize,
    pub from_snapshot: bool,
    pub ms: u128,
}

impl<E: Encoder> Engine<E> {
    /// `cache_dir`: where index snapshots live (one file pair per model fingerprint).
    pub fn new(root: impl Into<PathBuf>, cache_dir: Option<PathBuf>, encoder: E) -> Self {
        let dim = encoder.dim();
        Self {
            root: root.into(),
            cache_dir,
            encoder,
            index: Index::new(dim),
            lifecycle: IndexLifecycle::default(),
        }
    }

    pub fn index_state(&self) -> IndexState {
        self.lifecycle.state()
    }

    pub fn documents(&self) -> usize {
        self.index.len()
    }

    pub fn encoder_mut(&mut self) -> &mut E {
        &mut self.encoder
    }

    fn step(&mut self, event: IndexEvent) -> Result<(), EngineError> {
        self.lifecycle
            .handle(event)
            .map(|_| ())
            .map_err(|e| EngineError::Index(e.to_string()))
    }

    /// First build: loads the snapshot for this model if one exists, then brings it up to date.
    pub fn build(&mut self) -> Result<RefreshStats, EngineError> {
        self.step(IndexEvent::Init)?;
        let fingerprint = self.encoder.fingerprint();
        let snapshot = self
            .cache_dir
            .as_deref()
            .and_then(|d| Index::load(d, &fingerprint))
            .filter(|i| i.dim == self.encoder.dim());
        let from_snapshot = snapshot.is_some();
        self.index = snapshot.unwrap_or_else(|| Index::new(self.encoder.dim()));
        match self.sync() {
            Ok(mut stats) => {
                self.step(IndexEvent::IndexDone)?;
                stats.from_snapshot = from_snapshot;
                Ok(stats)
            }
            Err(err) => {
                self.step(IndexEvent::IndexFailed)?;
                Err(err)
            }
        }
    }

    /// Re-embeds changed and new files and drops deleted ones. Serves the old snapshot meanwhile.
    pub fn refresh(&mut self) -> Result<RefreshStats, EngineError> {
        let plan = plan_changes(&self.root, &self.known_keys())?;
        self.refresh_with(plan)
    }

    fn sync(&mut self) -> Result<RefreshStats, EngineError> {
        let plan = plan_changes(&self.root, &self.known_keys())?;
        self.apply(plan)
    }

    /// Owned copy of the indexed files and their change keys, for planning outside a lock.
    pub fn known_keys(&self) -> HashMap<String, FileKey> {
        self.index
            .keys()
            .into_iter()
            .map(|(p, k)| (p.to_string(), k))
            .collect()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Applies a refresh plan computed by [`plan_changes`] (possibly on another thread), walking
    /// the index lifecycle through Stale → Refreshing → Ready. An empty plan changes nothing.
    pub fn refresh_with(&mut self, plan: Plan) -> Result<RefreshStats, EngineError> {
        if plan.is_empty() && self.lifecycle.state() == IndexState::Ready {
            return Ok(RefreshStats {
                embedded: 0,
                removed: 0,
                documents: self.index.len(),
                from_snapshot: false,
                ms: 0,
            });
        }
        if matches!(
            self.lifecycle.state(),
            IndexState::Uninitialized | IndexState::Error
        ) {
            return self.build();
        }
        if self.lifecycle.state() == IndexState::Ready {
            self.step(IndexEvent::FilesChanged)?;
        }
        self.step(IndexEvent::Refresh)?;
        match self.apply(plan) {
            Ok(stats) => {
                self.step(IndexEvent::RefreshDone)?;
                Ok(stats)
            }
            Err(err) => {
                self.step(IndexEvent::RefreshFailed)?;
                Err(err)
            }
        }
    }

    fn apply(&mut self, plan: Plan) -> Result<RefreshStats, EngineError> {
        let start = Instant::now();
        let changed: HashSet<&str> = plan.todo.iter().map(|(f, _)| f.as_str()).collect();
        let before = self.index.len();
        self.index.retain_paths(&|e| {
            plan.current.contains(e.path.as_str()) && !changed.contains(e.path.as_str())
        });
        let removed = before - self.index.len();
        let mut docs = Vec::with_capacity(plan.todo.len());
        let mut entries = Vec::with_capacity(plan.todo.len());
        for (path, key) in &plan.todo {
            let Ok(text) = wn_sources::read_text(&self.root.join(path), MAX_FILE_BYTES) else {
                continue;
            };
            let doc = wn_sources::file_doc(path, &text);
            docs.push(doc.clone());
            entries.push(Entry {
                path: path.clone(),
                key: *key,
                doc,
            });
        }
        for (chunk_docs, chunk_entries) in docs.chunks(64).zip(entries.chunks(64)) {
            let vectors = self
                .encoder
                .encode_documents(chunk_docs)
                .map_err(EngineError::Encode)?;
            for (entry, vector) in chunk_entries.iter().zip(vectors) {
                self.index.push(entry.clone(), &vector);
            }
        }
        if let Some(dir) = &self.cache_dir {
            if !entries.is_empty() || removed > 0 {
                self.index
                    .save(dir, &self.encoder.fingerprint())
                    .map_err(|e| EngineError::Index(e.to_string()))?;
            }
        }
        Ok(RefreshStats {
            embedded: entries.len(),
            removed,
            documents: self.index.len(),
            from_snapshot: false,
            ms: start.elapsed().as_millis(),
        })
    }

    /// Top hints for a query. Requires a serving index state.
    pub fn ask(&mut self, query: &str, context: &str, k: usize) -> Result<Vec<Hint>, EngineError> {
        if !self.lifecycle.state().can_serve() {
            return Err(EngineError::Index(format!(
                "index not serving (state {:?})",
                self.lifecycle.state()
            )));
        }
        let q = self
            .encoder
            .encode_query(query, context)
            .map_err(EngineError::Encode)?;
        Ok(self
            .index
            .search(&q, k.min(MAX_HINTS))
            .into_iter()
            .map(|(i, similarity)| {
                let e = &self.index.entries[i];
                Hint {
                    path: e.path.clone(),
                    similarity,
                    reason: reason(query, &e.doc),
                }
            })
            .collect())
    }

    pub fn provenance(&self) -> Provenance {
        Provenance {
            model: self.encoder.name(),
            model_fingerprint: self.encoder.fingerprint(),
            index_state: format!("{:?}", self.lifecycle.state()),
            documents: self.index.len(),
            adapter_applied: false,
            ranking: "cosine",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reason_prefers_matching_names() {
        let doc = "file: src/upload.rs\nUpload chunks to S3\nupload_part retry_backoff Client";
        assert_eq!(
            reason("retry the upload when S3 times out", doc),
            "defines upload_part, retry_backoff"
        );
        assert_eq!(reason("unrelated", doc), "about: Upload chunks to S3");
        assert_eq!(
            reason("x", "file: a.rs\nmain"),
            "similar to the task description"
        );
    }

    #[test]
    fn hint_budget_is_three() {
        assert_eq!(MAX_HINTS, 3);
    }
}
