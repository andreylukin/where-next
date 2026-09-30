//! One repository's where-next state: scan, index, personal adapter and queries, built on the
//! `wn-core` runtime and the `wn-git` scanner.
//!
//! Scanning (git listing, stat calls) is a free function so it can run without holding the
//! daemon lock; only applying a changed scan (embedding new file versions) needs the workspace.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::Serialize;
use wn_core::adapter::{AdapterParams, ADAPTER_COMMITS};
use wn_core::encoder::Encoder;
use wn_core::index::{EntryKind, Index, IndexedFile, RefreshStats};
use wn_core::index_lifecycle::IndexState;
use wn_core::rank::Outcome;
use wn_core::runtime::{
    fit_from_history, load_adapter, save_adapter, suggest_with_exact, HistoryExample,
    StoredAdapter, SuggestOptions,
};
use wn_git::Coverage;

/// Largest file read for its skeleton.
pub const MAX_FILE_BYTES: usize = 400_000;

/// A shareable encoder.
pub type SharedEncoder = Arc<dyn Encoder + Send + Sync>;

/// Result of one scan: the indexable files with their version ids, and coverage counts.
#[derive(Debug, Clone, PartialEq)]
pub struct Scan {
    pub files: Vec<IndexedFile>,
    pub coverage: Coverage,
}

impl Scan {
    /// Path → version id, to detect changes between scans.
    pub fn versions(&self) -> BTreeMap<&str, &str> {
        self.files
            .iter()
            .map(|f| (f.path.as_str(), f.cid.as_str()))
            .collect()
    }
}

/// Scans `root` (tracked + untracked, deletions dropped). Safe to call without any lock.
pub fn scan(root: &Path) -> Scan {
    let (files, coverage) = wn_git::scan(root);
    Scan {
        files: files
            .into_iter()
            .map(|(path, e)| IndexedFile {
                path,
                cid: e.id.as_str().to_string(),
                kind: e.kind,
            })
            .collect(),
        coverage,
    }
}

/// Directory for a repository's index and adapter under `cache_home`: a hash of the absolute
/// root, so two checkouts never share a cache.
pub fn repo_cache_dir(cache_home: &Path, root: &Path) -> PathBuf {
    let abs = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    let mut h: u64 = 1469598103934665603;
    for b in abs.to_string_lossy().bytes() {
        h = (h ^ b as u64).wrapping_mul(1099511628211);
    }
    let name = abs
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    cache_home.join(format!("{name}-{h:016x}"))
}

/// Directory for one model's index and adapter for a repository: [`repo_cache_dir`] plus the
/// encoder fingerprint made path-safe, so switching models never mixes vectors. The `wn` CLI
/// and the MCP server both use this layout and therefore share one index.
pub fn model_cache_dir(cache_home: &Path, root: &Path, fingerprint: &str) -> PathBuf {
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
    repo_cache_dir(cache_home, root).join(tag)
}

/// File in a repository's cache directory naming its root, so a parent directory can find the
/// indexes of the repositories below it (see [`cached_repos`]).
pub const ROOT_FILE: &str = "root.json";

#[derive(Serialize, serde::Deserialize)]
struct RootFile {
    root: PathBuf,
}

/// Records `root` in its cache directory `repo_dir` (once; errors are ignored).
pub fn record_root(repo_dir: &Path, root: &Path) {
    let path = repo_dir.join(ROOT_FILE);
    if path.exists() {
        return;
    }
    let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
    if let Ok(bytes) = serde_json::to_vec(&RootFile { root }) {
        let _ = std::fs::create_dir_all(repo_dir);
        let _ = std::fs::write(path, bytes);
    }
}

/// The repository root recorded in a cache directory: [`ROOT_FILE`], else the usage log's copy
/// (caches from before [`ROOT_FILE`] existed).
pub fn cached_root(repo_dir: &Path) -> Option<PathBuf> {
    [ROOT_FILE, crate::usage::REPO_META]
        .iter()
        .find_map(|name| {
            let bytes = std::fs::read(repo_dir.join(name)).ok()?;
            serde_json::from_slice::<RootFile>(&bytes)
                .ok()
                .map(|f| f.root)
        })
}

/// Roots of every repository with a cache directory under `cache_home` whose recorded root still
/// maps to that directory (a moved or deleted checkout is skipped).
pub fn cached_repos(cache_home: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(cache_home) else {
        return Vec::new();
    };
    let mut roots: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let root = cached_root(&e.path())?;
            (root.is_dir() && repo_cache_dir(cache_home, &root) == e.path()).then_some(root)
        })
        .collect();
    roots.sort();
    roots
}

/// Where an answer came from.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Provenance {
    pub model: String,
    pub index_state: String,
    pub files_indexed: usize,
    pub configs_indexed: usize,
    pub coverage: Coverage,
}

/// What a refresh or warm-up did.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Refreshed {
    pub encoded: usize,
    pub removed: usize,
    pub files_indexed: usize,
    pub adapter: Option<String>,
    pub ms: u128,
}

pub struct Workspace {
    root: PathBuf,
    dir: PathBuf,
    encoder: SharedEncoder,
    index: Index,
    adapter: Option<StoredAdapter>,
    last_scan: Option<Scan>,
    /// One indexer per repository across processes (shared with the `wn` CLI and daemon).
    indexer: crate::indexer::Indexer,
    /// The stored revision this copy of the index reflects (see [`crate::indexer`]).
    revision: String,
    pub options: SuggestOptions,
    /// Minimum source files for task-start hints (see [`Workspace::ask_as`]).
    pub start_min_files: usize,
}

impl Workspace {
    /// Opens the stored index and adapter for this repository and model (nothing is embedded
    /// yet; call [`Workspace::apply`] with a scan).
    pub fn open(root: &Path, cache_home: &Path, encoder: SharedEncoder) -> Self {
        let dir = model_cache_dir(cache_home, root, &encoder.fingerprint());
        let indexer = crate::indexer::Indexer::new(&dir);
        // Read before loading: a change stored in between then triggers a reload.
        let revision = indexer.revision();
        let index = Index::open(&dir.join("index"), &encoder.fingerprint());
        let adapter =
            load_adapter(&dir.join("adapter")).filter(|a| a.meta.base == encoder.fingerprint());
        Self {
            root: root.to_path_buf(),
            dir,
            encoder,
            index,
            adapter,
            last_scan: None,
            indexer,
            revision,
            options: SuggestOptions::default(),
            start_min_files: wn_core::rank::START_HINT_MIN_FILES,
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// This repository's cache directory (shared by all models; holds the usage log).
    pub fn repo_dir(&self) -> Option<&Path> {
        self.dir.parent()
    }

    /// Encoder fingerprint.
    pub fn fingerprint(&self) -> String {
        self.encoder.fingerprint()
    }

    pub fn index_state(&self) -> IndexState {
        self.index.state()
    }

    /// Versions from the last applied scan (for cheap change detection outside the lock).
    pub fn last_versions(&self) -> BTreeMap<String, String> {
        self.last_scan
            .as_ref()
            .map(|s| {
                s.files
                    .iter()
                    .map(|f| (f.path.clone(), f.cid.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Embeds new or changed file versions from `scan` and drops the rest, as the repository's
    /// only indexer: if another one (a `wn init`, the daemon) is busy, this waits for it and
    /// reloads what it stored, so nothing is embedded twice.
    pub fn apply(&mut self, scan: Scan) -> Result<RefreshStats, String> {
        let waited = self
            .indexer
            .begin(|| {})
            .map_err(|e| format!("cannot write to {}: {e}", self.dir.display()))?;
        if waited || self.indexer.revision() != self.revision {
            // Another indexer stored something newer since this copy was loaded.
            self.revision = self.indexer.revision();
            self.index = Index::open(&self.dir.join("index"), &self.encoder.fingerprint());
        }
        let result = self.apply_locked(scan);
        if let (Ok(_), Some(repo_dir)) = (&result, self.dir.parent()) {
            record_root(repo_dir, &self.root);
        }
        let changed = match &result {
            Ok(stats) => stats.encoded > 0 || stats.removed > 0,
            Err(_) => true,
        };
        if changed && self.indexer.mark_changed().is_ok() {
            self.revision = self.indexer.revision();
        }
        self.indexer.finish(result.is_ok());
        result
    }

    fn apply_locked(&mut self, scan: Scan) -> Result<RefreshStats, String> {
        let root = self.root.clone();
        let read = move |path: &str, _kind: wn_sources::Kind| {
            wn_sources::read_text(&root.join(path), MAX_FILE_BYTES).ok()
        };
        let stats = self
            .index
            .refresh(&scan.files, &read, self.encoder.as_ref(), false)
            .map_err(|e| e.to_string())?;
        self.last_scan = Some(scan);
        Ok(stats)
    }

    /// Fits the personal adapter from commit history when none matches the current model.
    /// Returns the adapter revision in use, or why none could be fitted.
    pub fn ensure_adapter(&mut self) -> Result<String, String> {
        if let Some(a) = &self.adapter {
            return Ok(a.meta.revision.clone());
        }
        let examples: Vec<HistoryExample> = wn_git::history(&self.root, ADAPTER_COMMITS)
            .into_iter()
            .map(|c| HistoryExample {
                sha: c.sha,
                date: c.date,
                subject: c.subject,
                body: c.body,
                paths: c.paths,
            })
            .collect();
        let adapter = fit_from_history(
            &self.index,
            &examples,
            &[],
            self.encoder.as_ref(),
            &AdapterParams::default(),
            ADAPTER_COMMITS,
            0,
        )
        .map_err(|e| e.to_string())?;
        save_adapter(&self.dir.join("adapter"), &adapter).map_err(|e| e.to_string())?;
        let revision = adapter.meta.revision.clone();
        self.adapter = Some(adapter);
        Ok(revision)
    }

    /// Scan, apply and (if possible) fit the adapter.
    pub fn warm(&mut self) -> Result<Refreshed, String> {
        let start = Instant::now();
        let stats = self.apply(scan(&self.root))?;
        let adapter = self.ensure_adapter().ok();
        Ok(Refreshed {
            encoded: stats.encoded,
            removed: stats.removed,
            files_indexed: self.index.count(EntryKind::File),
            adapter,
            ms: start.elapsed().as_millis(),
        })
    }

    /// Answers a query. Operational problems come back as fail-open outcome states.
    pub fn ask(&self, query: &str, context: &str) -> Outcome {
        self.ask_as(query, context, false)
    }

    /// [`Workspace::ask`]; a task-start query (`start`) is skipped in repositories with fewer
    /// than [`Workspace::start_min_files`] source files.
    pub fn ask_as(&self, query: &str, context: &str, start: bool) -> Outcome {
        let mut opts = self.options;
        if start {
            opts.start_min_files = Some(self.start_min_files);
        }
        opts.unsupported_only = self
            .last_scan
            .as_ref()
            .is_some_and(|s| s.files.is_empty() && s.coverage.unsupported > 0);
        suggest_with_exact(
            &self.index,
            self.adapter.as_ref(),
            self.encoder.as_ref(),
            query,
            context,
            opts,
            Some(&self.root),
        )
    }

    pub fn provenance(&self) -> Provenance {
        Provenance {
            model: self.encoder.fingerprint(),
            index_state: format!("{:?}", self.index.state()),
            files_indexed: self.index.count(EntryKind::File),
            configs_indexed: self.index.count(EntryKind::Config),
            coverage: self
                .last_scan
                .as_ref()
                .map(|s| s.coverage.clone())
                .unwrap_or_default(),
        }
    }
}

#[cfg(test)]
mod cache_dir_tests {
    use super::*;

    #[test]
    fn model_dirs_are_per_repo_and_per_model() {
        let home = Path::new("/tmp/wn-home");
        let a = model_cache_dir(home, Path::new("/nonexistent/proj"), "gemma-g2r-abc/q8");
        let b = model_cache_dir(home, Path::new("/nonexistent/proj"), "v2b-abc");
        let c = model_cache_dir(home, Path::new("/nonexistent/other/proj"), "v2b-abc");
        assert_eq!(a.parent(), b.parent());
        assert_ne!(a, b);
        assert_ne!(b.parent(), c.parent());
        assert!(a.ends_with("gemma-g2r-abc_q8"));
    }

    #[test]
    fn cache_dirs_map_back_to_their_roots() {
        let home = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        let root = repo.path().canonicalize().unwrap();
        let dir = repo_cache_dir(home.path(), &root);
        assert!(cached_repos(home.path()).is_empty());
        record_root(&dir, &root);
        assert_eq!(cached_root(&dir), Some(root.clone()));
        assert_eq!(cached_repos(home.path()), vec![root.clone()]);
        // Caches from before `root.json` fall back to the usage log's copy.
        std::fs::remove_file(dir.join(ROOT_FILE)).unwrap();
        let meta = serde_json::json!({ "root": root });
        std::fs::write(dir.join(crate::usage::REPO_META), meta.to_string()).unwrap();
        assert_eq!(cached_repos(home.path()), vec![root.clone()]);
        // A directory whose recorded root maps elsewhere (a moved checkout) is skipped.
        let stray = home.path().join("stray-0000000000000000");
        record_root(&stray, &root);
        assert_eq!(cached_repos(home.path()), vec![root]);
    }
}
