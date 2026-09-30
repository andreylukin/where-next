//! A repository's vector index: one vector per source file, config file and (optionally)
//! definition, cached on disk per embedding model and refreshed incrementally.
//!
//! Only files whose version id changed are re-embedded. Documents are embedded in chunks and
//! the index is checkpointed after each chunk, so an interrupted first build resumes where it
//! stopped; a checkpoint of an unfinished first build is marked incomplete and never serves.
//! Snapshots are written atomically (new files, then rename), so a reader never sees a
//! half-written index. The lifecycle is driven through [`IndexLifecycle`], so an index that
//! failed to build can never serve results.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wn_sources::{config_doc, file_doc, function_docs, Kind, CONFIG_LINES};

use crate::encoder::{EncodeError, Encoder};
use crate::index_lifecycle::{IndexEvent, IndexLifecycle, IndexState};
use crate::rank::{auxiliary_dir, path_prior, round4, select_top, Hint};

/// Documents embedded between checkpoints by default.
pub const CHECKPOINT_EVERY: usize = 1024;

/// What an index entry stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    /// A source file.
    File,
    /// A config file.
    Config,
    /// A definition inside a source file.
    Function,
    /// Marker: this file version has no definitions (so it is not rescanned); never ranked.
    FunctionNone,
}

/// One indexed item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// What it is.
    pub kind: EntryKind,
    /// Repository-relative path.
    pub path: String,
    /// Version id of the file the vector was built from.
    pub cid: String,
    /// 1-based line (definitions) or 1 (files).
    pub line: usize,
    /// Definition name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// A file to index, as found by a repository scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    /// Repository-relative path.
    pub path: String,
    /// Version id (blob SHA or mtime+size).
    pub cid: String,
    /// Source or config.
    pub kind: Kind,
}

/// What a refresh did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct RefreshStats {
    /// Texts embedded.
    pub encoded: usize,
    /// Entries dropped (file changed or removed).
    pub removed: usize,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    fingerprint: String,
    dim: usize,
    entries: Vec<Entry>,
    /// False for a checkpoint of an unfinished first build (older snapshots are complete).
    #[serde(default = "complete_by_default")]
    complete: bool,
}

fn complete_by_default() -> bool {
    true
}

/// A repository index for one embedding model.
#[derive(Debug)]
pub struct Index {
    dir: PathBuf,
    fingerprint: String,
    entries: Vec<Entry>,
    vecs: Vec<f32>,
    dim: usize,
    lifecycle: IndexLifecycle,
    checkpoint_every: usize,
}

impl Index {
    /// Opens (or starts) the index stored in `dir` for the model `fingerprint`. A stored index
    /// built by a different model is ignored.
    pub fn open(dir: &Path, fingerprint: &str) -> Index {
        let mut index = Index {
            dir: dir.to_path_buf(),
            fingerprint: fingerprint.to_string(),
            entries: Vec::new(),
            vecs: Vec::new(),
            dim: 0,
            lifecycle: IndexLifecycle::default(),
            checkpoint_every: CHECKPOINT_EVERY,
        };
        if let Some((meta, vecs)) = load(dir) {
            if meta.fingerprint == fingerprint && vecs.len() == meta.entries.len() * meta.dim {
                index.entries = meta.entries;
                index.vecs = vecs;
                index.dim = meta.dim;
                if meta.complete {
                    // A completed build; the next refresh brings it up to date.
                    let _ = index.lifecycle.handle(IndexEvent::Init);
                    let _ = index.lifecycle.handle(IndexEvent::IndexDone);
                }
                // An incomplete checkpoint stays Uninitialized: its vectors are reused by the
                // next refresh, which finishes the build before anything is served.
            }
        }
        index
    }

    /// Documents to embed between checkpoints (at least 1).
    pub fn set_checkpoint_every(&mut self, n: usize) {
        self.checkpoint_every = n.max(1);
    }

    /// Lifecycle state.
    pub fn state(&self) -> IndexState {
        self.lifecycle.state()
    }

    /// The model fingerprint this index belongs to.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// Vector dimension (0 when empty).
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// Number of entries of a kind.
    pub fn count(&self, kind: EntryKind) -> usize {
        self.entries.iter().filter(|e| e.kind == kind).count()
    }

    /// Paths currently represented by this index kind.
    pub fn paths(&self, kind: EntryKind) -> impl Iterator<Item = &str> {
        self.entries
            .iter()
            .filter(move |e| e.kind == kind)
            .map(|e| e.path.as_str())
    }

    /// Entries and their vectors (row-major) for one kind.
    pub fn matrix(&self, kind: EntryKind) -> (Vec<&Entry>, Vec<f32>) {
        let mut rows = Vec::new();
        let mut mat = Vec::new();
        for (i, e) in self.entries.iter().enumerate() {
            if e.kind == kind {
                rows.push(e);
                mat.extend_from_slice(&self.vecs[i * self.dim..(i + 1) * self.dim]);
            }
        }
        (rows, mat)
    }

    /// Brings the index up to date with `files`, embedding only new or changed versions.
    /// `read` returns a file's text (or `None` if it cannot be read).
    pub fn refresh(
        &mut self,
        files: &[IndexedFile],
        read: &dyn Fn(&str, Kind) -> Option<String>,
        encoder: &dyn Encoder,
        with_functions: bool,
    ) -> Result<RefreshStats, EncodeError> {
        let wanted: HashMap<&str, &IndexedFile> =
            files.iter().map(|f| (f.path.as_str(), f)).collect();
        let mut keep = Vec::new();
        let mut have: HashSet<(bool, &str, &str)> = HashSet::new();
        for (i, e) in self.entries.iter().enumerate() {
            if wanted.get(e.path.as_str()).is_some_and(|f| f.cid == e.cid) {
                keep.push(i);
                let is_func = matches!(e.kind, EntryKind::Function | EntryKind::FunctionNone);
                have.insert((is_func, e.path.as_str(), e.cid.as_str()));
            }
        }
        let mut new_entries = Vec::new();
        let mut texts: Vec<Option<String>> = Vec::new();
        for f in files {
            let need_doc = !have.contains(&(false, f.path.as_str(), f.cid.as_str()));
            let need_func = with_functions
                && f.kind == Kind::Source
                && !have.contains(&(true, f.path.as_str(), f.cid.as_str()));
            if !(need_doc || need_func) {
                continue;
            }
            let Some(raw) = read(&f.path, f.kind) else {
                continue;
            };
            if need_doc {
                let (kind, text) = match f.kind {
                    Kind::Source => (EntryKind::File, file_doc(&f.path, &raw)),
                    Kind::Config => (EntryKind::Config, config_doc(&f.path, &raw, CONFIG_LINES)),
                };
                new_entries.push(Entry {
                    kind,
                    path: f.path.clone(),
                    cid: f.cid.clone(),
                    line: 1,
                    name: None,
                });
                texts.push(Some(text));
            }
            if need_func {
                let funcs = function_docs(&f.path, &raw);
                if funcs.is_empty() {
                    new_entries.push(Entry {
                        kind: EntryKind::FunctionNone,
                        path: f.path.clone(),
                        cid: f.cid.clone(),
                        line: 0,
                        name: None,
                    });
                    texts.push(None);
                }
                for (name, line, text) in funcs {
                    new_entries.push(Entry {
                        kind: EntryKind::Function,
                        path: f.path.clone(),
                        cid: f.cid.clone(),
                        line,
                        name: Some(name),
                    });
                    texts.push(Some(text));
                }
            }
        }
        let removed = self.entries.len() - keep.len();
        if removed == 0 && texts.is_empty() {
            if self.state() == IndexState::Uninitialized {
                // An empty repository is a completed (empty) build.
                self.transition(IndexEvent::Init);
                self.transition(IndexEvent::IndexDone);
            }
            return Ok(RefreshStats::default());
        }
        let first_build = !self.state().can_serve();
        if first_build {
            self.transition(IndexEvent::Init);
        } else {
            self.transition(IndexEvent::FilesChanged);
            self.transition(IndexEvent::Refresh);
        }
        let encoded_total = texts.iter().flatten().count();
        // Kept entries first, then new ones as their chunk is embedded.
        let mut vecs = Vec::with_capacity((keep.len() + texts.len()) * self.dim.max(1));
        let mut entries = Vec::with_capacity(keep.len() + new_entries.len());
        for &i in &keep {
            entries.push(self.entries[i].clone());
            vecs.extend_from_slice(&self.vecs[i * self.dim..(i + 1) * self.dim]);
        }
        let mut dim = self.dim;
        let mut pending = new_entries.into_iter().zip(texts).peekable();
        while pending.peek().is_some() {
            // One chunk: up to `checkpoint_every` texts, plus any text-less markers among them.
            let mut chunk = Vec::new();
            let mut n_texts = 0;
            while let Some((_, text)) = pending.peek() {
                if text.is_some() && n_texts == self.checkpoint_every {
                    break;
                }
                n_texts += usize::from(text.is_some());
                chunk.push(pending.next().expect("peeked"));
            }
            let batch: Vec<String> = chunk.iter().filter_map(|(_, t)| t.clone()).collect();
            let result = if batch.is_empty() {
                Ok(Vec::new())
            } else {
                encoder.documents(&batch)
            };
            let embedded = match result {
                Ok(v) => v,
                Err(e) => {
                    self.fail(first_build);
                    return Err(e);
                }
            };
            let chunk_dim = embedded.first().map(Vec::len).unwrap_or(dim.max(1));
            if dim != 0 && chunk_dim != dim {
                self.fail(first_build);
                return Err(EncodeError(format!(
                    "dimension changed from {dim} to {chunk_dim}"
                )));
            }
            if dim == 0 {
                dim = chunk_dim;
            }
            let mut next = embedded.into_iter();
            for (entry, text) in chunk {
                entries.push(entry);
                match text {
                    Some(_) => vecs.extend(next.next().unwrap_or_else(|| vec![0.0; dim])),
                    None => vecs.extend(std::iter::repeat(0.0).take(dim)),
                }
            }
            if pending.peek().is_some() {
                // Checkpoint: an unfinished first build is saved as incomplete.
                let _ = save(
                    &self.dir,
                    &self.fingerprint,
                    dim,
                    &entries,
                    &vecs,
                    !first_build,
                );
            }
        }
        self.entries = entries;
        self.vecs = vecs;
        self.dim = dim.max(1);
        let saved = save(
            &self.dir,
            &self.fingerprint,
            self.dim,
            &self.entries,
            &self.vecs,
            true,
        );
        self.transition(match (first_build, saved.is_ok()) {
            (true, true) => IndexEvent::IndexDone,
            (true, false) => IndexEvent::IndexFailed,
            (false, true) => IndexEvent::RefreshDone,
            (false, false) => IndexEvent::RefreshFailed,
        });
        saved.map_err(|e| EncodeError(format!("saving index: {e}")))?;
        Ok(RefreshStats {
            encoded: encoded_total,
            removed,
        })
    }

    fn fail(&mut self, first_build: bool) {
        self.transition(if first_build {
            IndexEvent::IndexFailed
        } else {
            IndexEvent::RefreshFailed
        });
    }

    fn transition(&mut self, event: IndexEvent) {
        // The table covers every transition refresh() drives; a rejection here is a bug.
        self.lifecycle
            .handle(event)
            .unwrap_or_else(|e| panic!("index lifecycle: {e}"));
    }

    /// Top `k` entries of `kind` for an (already adapted, normalised) query vector.
    pub fn rank(&self, q: &[f32], kind: EntryKind, k: usize) -> Vec<Hint> {
        self.rank_for_query(q, kind, k, "", &[])
    }

    /// Rank with task-specific path and exact-literal evidence; displayed similarities stay raw.
    pub fn rank_for_query(
        &self,
        q: &[f32],
        kind: EntryKind,
        k: usize,
        query: &str,
        exact: &[String],
    ) -> Vec<Hint> {
        if self.dim == 0 || q.len() != self.dim {
            return Vec::new();
        }
        let d = self.dim;
        let file_count = self.count(EntryKind::File);
        let mostly_auxiliary = file_count > 0
            && self
                .paths(EntryKind::File)
                .filter(|p| auxiliary_dir(p))
                .count()
                * 2
                > file_count;
        let scored: Vec<(usize, f32)> = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| e.kind == kind)
            .map(|(i, _)| {
                let row = &self.vecs[i * d..(i + 1) * d];
                let raw: f32 = row.iter().zip(q).map(|(a, b)| a * b).sum();
                let path = &self.entries[i].path;
                let prior = if kind == EntryKind::File && !mostly_auxiliary {
                    path_prior(path, query)
                } else {
                    0.0
                };
                let exact_boost = if kind == EntryKind::File && exact.iter().any(|p| p == path) {
                    0.5
                } else {
                    0.0
                };
                (i, raw + prior + exact_boost)
            })
            .collect();
        select_top(scored, k)
            .into_iter()
            .map(|(i, _)| {
                let e = &self.entries[i];
                let func = kind == EntryKind::Function;
                let row = &self.vecs[i * d..(i + 1) * d];
                let score: f32 = row.iter().zip(q).map(|(a, b)| a * b).sum();
                Hint {
                    path: e.path.clone(),
                    similarity: round4(score),
                    evidence: (kind == EntryKind::File && exact.iter().any(|p| p == &e.path))
                        .then(|| "exact".to_string()),
                    name: if func { e.name.clone() } else { None },
                    line: if func { Some(e.line) } else { None },
                }
            })
            .collect()
    }
}

fn save(
    dir: &Path,
    fingerprint: &str,
    dim: usize,
    entries: &[Entry],
    vecs: &[f32],
    complete: bool,
) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let tag = format!("{}.{}", std::process::id(), nanos());
    let vtmp = dir.join(format!("vecs.{tag}.f32"));
    let mtmp = dir.join(format!("meta.{tag}.json"));
    let mut bytes = Vec::with_capacity(vecs.len() * 4);
    for x in vecs {
        bytes.extend_from_slice(&x.to_le_bytes());
    }
    fs::write(&vtmp, bytes)?;
    let meta = Meta {
        fingerprint: fingerprint.to_string(),
        dim,
        entries: entries.to_vec(),
        complete,
    };
    fs::write(&mtmp, serde_json::to_vec(&meta).map_err(io::Error::other)?)?;
    // Vectors first: `load` rejects a meta whose entry count does not match the vectors, so a
    // crash between the renames yields no index rather than a mismatched one.
    fs::rename(&vtmp, dir.join("vecs.f32"))?;
    fs::rename(&mtmp, dir.join("meta.json"))?;
    Ok(())
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn load(dir: &Path) -> Option<(Meta, Vec<f32>)> {
    let meta: Meta = serde_json::from_slice(&fs::read(dir.join("meta.json")).ok()?).ok()?;
    let raw = fs::read(dir.join("vecs.f32")).ok()?;
    if raw.len() % 4 != 0 {
        return None;
    }
    let vecs = raw
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect();
    Some((meta, vecs))
}
