//! A repository's vector index: one vector per source file, config file and (optionally)
//! definition, cached on disk per embedding model and refreshed incrementally.
//!
//! Only files whose version id changed are re-embedded. Snapshots are written atomically (new
//! files, then rename), so a reader never sees a half-written index. The lifecycle is driven
//! through [`IndexLifecycle`], so an index that failed to build can never serve results.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wn_sources::{config_doc, file_doc, function_docs, Kind, CONFIG_LINES};

use crate::encoder::{EncodeError, Encoder};
use crate::index_lifecycle::{IndexEvent, IndexLifecycle, IndexState};
use crate::rank::{round4, top_k, Hint};

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
        };
        if let Some((meta, vecs)) = load(dir) {
            if meta.fingerprint == fingerprint && vecs.len() == meta.entries.len() * meta.dim {
                index.entries = meta.entries;
                index.vecs = vecs;
                index.dim = meta.dim;
                // A stored snapshot is a completed build; the next refresh brings it up to date.
                let _ = index.lifecycle.handle(IndexEvent::Init);
                let _ = index.lifecycle.handle(IndexEvent::IndexDone);
            }
        }
        index
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
        let to_encode: Vec<String> = texts.iter().flatten().cloned().collect();
        let encoded = if to_encode.is_empty() {
            Vec::new()
        } else {
            match encoder.documents(&to_encode) {
                Ok(v) => v,
                Err(e) => {
                    self.transition(if first_build {
                        IndexEvent::IndexFailed
                    } else {
                        IndexEvent::RefreshFailed
                    });
                    return Err(e);
                }
            }
        };
        let dim = encoded
            .first()
            .map(Vec::len)
            .unwrap_or(if self.dim > 0 { self.dim } else { 1 });
        if self.dim != 0 && dim != self.dim {
            self.transition(if first_build {
                IndexEvent::IndexFailed
            } else {
                IndexEvent::RefreshFailed
            });
            return Err(EncodeError(format!(
                "dimension changed from {} to {dim}",
                self.dim
            )));
        }
        let mut vecs = Vec::with_capacity((keep.len() + texts.len()) * dim);
        let mut entries = Vec::with_capacity(keep.len() + new_entries.len());
        for &i in &keep {
            entries.push(self.entries[i].clone());
            vecs.extend_from_slice(&self.vecs[i * self.dim..(i + 1) * self.dim]);
        }
        let mut next = encoded.into_iter();
        for (entry, text) in new_entries.into_iter().zip(&texts) {
            entries.push(entry);
            match text {
                Some(_) => vecs.extend(next.next().unwrap_or_else(|| vec![0.0; dim])),
                None => vecs.extend(std::iter::repeat(0.0).take(dim)),
            }
        }
        self.entries = entries;
        self.vecs = vecs;
        self.dim = dim;
        let saved = self.save();
        self.transition(match (first_build, saved.is_ok()) {
            (true, true) => IndexEvent::IndexDone,
            (true, false) => IndexEvent::IndexFailed,
            (false, true) => IndexEvent::RefreshDone,
            (false, false) => IndexEvent::RefreshFailed,
        });
        saved.map_err(|e| EncodeError(format!("saving index: {e}")))?;
        Ok(RefreshStats {
            encoded: to_encode.len(),
            removed,
        })
    }

    fn transition(&mut self, event: IndexEvent) {
        // The table covers every transition refresh() drives; a rejection here is a bug.
        self.lifecycle
            .handle(event)
            .unwrap_or_else(|e| panic!("index lifecycle: {e}"));
    }

    /// Top `k` entries of `kind` for an (already adapted, normalised) query vector.
    pub fn rank(&self, q: &[f32], kind: EntryKind, k: usize) -> Vec<Hint> {
        if self.dim == 0 || q.len() != self.dim {
            return Vec::new();
        }
        let (rows, mat) = self.matrix(kind);
        top_k(&mat, self.dim, q, k)
            .into_iter()
            .map(|(i, score)| {
                let e = rows[i];
                let func = kind == EntryKind::Function;
                Hint {
                    path: e.path.clone(),
                    similarity: round4(score),
                    name: if func { e.name.clone() } else { None },
                    line: if func { Some(e.line) } else { None },
                }
            })
            .collect()
    }

    fn save(&self) -> io::Result<()> {
        fs::create_dir_all(&self.dir)?;
        let tag = format!("{}.{}", std::process::id(), nanos());
        let vtmp = self.dir.join(format!("vecs.{tag}.f32"));
        let mtmp = self.dir.join(format!("meta.{tag}.json"));
        let mut bytes = Vec::with_capacity(self.vecs.len() * 4);
        for x in &self.vecs {
            bytes.extend_from_slice(&x.to_le_bytes());
        }
        fs::write(&vtmp, bytes)?;
        let meta = Meta {
            fingerprint: self.fingerprint.clone(),
            dim: self.dim,
            entries: self.entries.clone(),
        };
        fs::write(&mtmp, serde_json::to_vec(&meta).map_err(io::Error::other)?)?;
        fs::rename(&vtmp, self.dir.join("vecs.f32"))?;
        fs::rename(&mtmp, self.dir.join("meta.json"))?;
        Ok(())
    }
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
