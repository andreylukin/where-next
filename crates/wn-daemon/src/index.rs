//! In-memory document index: one unit vector per file, brute-force cosine search, and an on-disk
//! snapshot keyed by the model fingerprint.
//!
//! Brute force is the right first choice here: 20k files × 768 dims is ~15M multiply-adds per
//! query, a few milliseconds on a laptop, with exact results.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// Change detector for a file: size and modification time (nanoseconds since the epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileKey {
    pub size: u64,
    pub mtime_ns: u128,
}

impl FileKey {
    pub fn of(path: &Path) -> io::Result<FileKey> {
        let meta = fs::metadata(path)?;
        let mtime_ns = meta
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Ok(FileKey {
            size: meta.len(),
            mtime_ns,
        })
    }
}

/// One indexed document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: String,
    pub key: FileKey,
    /// The document text that was embedded (a `wn-sources` skeleton); used for hint reasons.
    pub doc: String,
}

/// Documents plus their vectors (row-major, `dim` floats per entry).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Index {
    pub dim: usize,
    pub entries: Vec<Entry>,
    pub vectors: Vec<f32>,
}

#[derive(Serialize, Deserialize)]
struct Meta {
    format: u32,
    fingerprint: String,
    dim: usize,
    entries: Vec<Entry>,
}

const FORMAT: u32 = 1;

impl Index {
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            ..Default::default()
        }
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn vector(&self, i: usize) -> &[f32] {
        &self.vectors[i * self.dim..(i + 1) * self.dim]
    }

    pub fn push(&mut self, entry: Entry, vector: &[f32]) {
        assert_eq!(vector.len(), self.dim, "vector dimension mismatch");
        self.entries.push(entry);
        self.vectors.extend_from_slice(vector);
    }

    /// Top `k` entries by dot product with a unit query vector, best first. Ties keep index order.
    pub fn search(&self, query: &[f32], k: usize) -> Vec<(usize, f32)> {
        let mut scored: Vec<(usize, f32)> = (0..self.len())
            .map(|i| {
                let score = self.vector(i).iter().zip(query).map(|(a, b)| a * b).sum();
                (i, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        scored.truncate(k);
        scored
    }

    /// Keeps entries whose path is in `keep`, dropping the rest (deleted or changed files).
    pub fn retain_paths(&mut self, keep: &dyn Fn(&Entry) -> bool) {
        let mut entries = Vec::with_capacity(self.entries.len());
        let mut vectors = Vec::with_capacity(self.vectors.len());
        for (i, e) in self.entries.iter().enumerate() {
            if keep(e) {
                entries.push(e.clone());
                vectors.extend_from_slice(self.vector(i));
            }
        }
        self.entries = entries;
        self.vectors = vectors;
    }

    pub fn keys(&self) -> HashMap<&str, FileKey> {
        self.entries
            .iter()
            .map(|e| (e.path.as_str(), e.key))
            .collect()
    }

    /// Writes the snapshot atomically: `<stem>.json` (metadata) and `<stem>.f32` (vectors),
    /// each written to a temporary file and renamed.
    pub fn save(&self, dir: &Path, fingerprint: &str) -> io::Result<()> {
        fs::create_dir_all(dir)?;
        let meta = Meta {
            format: FORMAT,
            fingerprint: fingerprint.to_string(),
            dim: self.dim,
            entries: self.entries.clone(),
        };
        let bytes: Vec<u8> = self.vectors.iter().flat_map(|x| x.to_le_bytes()).collect();
        let stem = dir.join(fingerprint);
        write_atomic(&stem.with_extension("f32"), &bytes)?;
        write_atomic(&stem.with_extension("json"), &serde_json::to_vec(&meta)?)
    }

    /// Loads a snapshot written by [`Index::save`] for this fingerprint, if present and intact.
    pub fn load(dir: &Path, fingerprint: &str) -> Option<Index> {
        let stem = dir.join(fingerprint);
        let meta: Meta =
            serde_json::from_slice(&fs::read(stem.with_extension("json")).ok()?).ok()?;
        if meta.format != FORMAT || meta.fingerprint != fingerprint {
            return None;
        }
        let bytes = fs::read(stem.with_extension("f32")).ok()?;
        if bytes.len() != meta.entries.len() * meta.dim * 4 {
            return None;
        }
        let vectors = bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        Some(Index {
            dim: meta.dim,
            entries: meta.entries,
            vectors,
        })
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = path.with_extension(format!(
        "{}.tmp{}",
        path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        std::process::id()
    ));
    fs::write(&tmp, bytes)?;
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn entry(path: &str) -> Entry {
        Entry {
            path: path.into(),
            key: FileKey {
                size: 1,
                mtime_ns: 1,
            },
            doc: format!("file: {path}"),
        }
    }

    fn unit(v: &[f32]) -> Vec<f32> {
        let n = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / n).collect()
    }

    #[test]
    fn search_ranks_by_similarity() {
        let mut index = Index::new(2);
        index.push(entry("a"), &unit(&[1.0, 0.0]));
        index.push(entry("b"), &unit(&[0.0, 1.0]));
        index.push(entry("c"), &unit(&[1.0, 1.0]));
        let hits = index.search(&unit(&[1.0, 0.2]), 2);
        assert_eq!(hits.iter().map(|h| h.0).collect::<Vec<_>>(), vec![0, 2]);
    }

    #[test]
    fn save_load_roundtrip_and_fingerprint_guard() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = Index::new(3);
        index.push(entry("x"), &unit(&[1.0, 2.0, 3.0]));
        index.save(dir.path(), "abc").unwrap();
        assert_eq!(Index::load(dir.path(), "abc").unwrap(), index);
        assert!(Index::load(dir.path(), "other").is_none());
    }

    #[test]
    fn truncated_snapshot_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut index = Index::new(2);
        index.push(entry("x"), &unit(&[1.0, 2.0]));
        index.save(dir.path(), "fp").unwrap();
        fs::write(dir.path().join("fp.f32"), [0u8; 3]).unwrap();
        assert!(Index::load(dir.path(), "fp").is_none());
    }

    #[test]
    fn retain_drops_vectors_with_entries() {
        let mut index = Index::new(1);
        index.push(entry("keep"), &[1.0]);
        index.push(entry("drop"), &[-1.0]);
        index.retain_paths(&|e| e.path == "keep");
        assert_eq!(index.len(), 1);
        assert_eq!(index.vectors, vec![1.0]);
    }

    proptest! {
        #[test]
        fn search_is_sorted_and_bounded(
            rows in prop::collection::vec(prop::collection::vec(-1.0f32..1.0, 4), 0..40),
            q in prop::collection::vec(-1.0f32..1.0, 4),
            k in 0usize..10,
        ) {
            let mut index = Index::new(4);
            for (i, r) in rows.iter().enumerate() {
                index.push(entry(&i.to_string()), r);
            }
            let hits = index.search(&q, k);
            prop_assert_eq!(hits.len(), k.min(rows.len()));
            for pair in hits.windows(2) {
                prop_assert!(pair[0].1 >= pair[1].1);
            }
        }
    }
}
