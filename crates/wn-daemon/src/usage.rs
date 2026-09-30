//! Local usage log: what `wn report` summarises. It never leaves the machine on its own.
//!
//! Each repository's cache directory (see [`crate::workspace::repo_cache_dir`]) holds:
//! - `usage.jsonl`: one [`QueryEvent`] per answered query, pruned to [`RETENTION_DAYS`];
//! - `usage-index.json`: the latest [`IndexEvent`] (index size, timing, language mix);
//! - `usage-bench.json`: the latest [`BenchEvent`] (`wn bench --history` hit rates);
//! - `usage-repo.json`: the repository root, so the report can check later edits locally.
//!
//! Setting `WN_NO_LOG` (to anything) turns all of this off. Paths and queries stay local: the
//! report reduces them to buckets and rates before anything is shown or shared.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Days of query events kept.
pub const RETENTION_DAYS: u64 = 30;
/// Query log file name.
pub const QUERY_LOG: &str = "usage.jsonl";
/// Latest index stats.
pub const INDEX_STATS: &str = "usage-index.json";
/// Latest bench stats.
pub const BENCH_STATS: &str = "usage-bench.json";
/// Repository root (local only).
pub const REPO_META: &str = "usage-repo.json";

const DAY: u64 = 86_400;

/// Seconds since the Unix epoch.
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Whether logging is on (`WN_NO_LOG` unset).
pub fn enabled() -> bool {
    std::env::var_os("WN_NO_LOG").is_none()
}

/// One answered query.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueryEvent {
    /// When (Unix seconds).
    pub ts: u64,
    /// Query kind label (`request`, `issue`, `error`, `conversational`).
    pub kind: String,
    /// Answer state (`ok`, `abstain`, `empty_index`, …).
    pub state: String,
    /// Answer latency in milliseconds.
    pub ms: u64,
    /// Encoder fingerprint.
    pub model: String,
    /// Whether the personal adapter was applied.
    pub adapter: bool,
    /// Source files indexed at the time.
    pub files: usize,
    /// Hinted file paths (local only; the report checks whether they were edited later).
    #[serde(default)]
    pub hinted: Vec<String>,
}

/// The latest index build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct IndexEvent {
    /// When (Unix seconds).
    pub ts: u64,
    /// Wall time of the scan + index refresh, in milliseconds.
    pub ms: u64,
    /// Indexed source files.
    pub files: usize,
    /// Indexed config files.
    pub configs: usize,
    /// Indexed source files by lowercased extension (local only; reduced to a language mix).
    #[serde(default)]
    pub extensions: BTreeMap<String, usize>,
    /// Commits the adapter was fitted on (0 without an adapter).
    pub history_commits: usize,
    /// Peak resident memory of the process, in megabytes, when known.
    #[serde(default)]
    pub peak_mb: Option<u64>,
    /// Encoder fingerprint.
    pub model: String,
}

/// The latest `wn bench --history` result (hit rates only).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct BenchEvent {
    /// When (Unix seconds).
    pub ts: u64,
    /// Encoder fingerprint or model name.
    pub model: String,
    /// Median candidate files per scored commit.
    pub files_median: usize,
    /// Commits scored (0 in logs written before this field existed).
    #[serde(default)]
    pub tasks: usize,
    /// hit@1, hit@3, hit@10 for the lexical baseline.
    pub lexical: [f64; 3],
    /// hit@1, hit@3, hit@10 for the model.
    pub model_hits: [f64; 3],
    /// hit@1, hit@3, hit@10 for model + adapter, when an adapter was fitted.
    #[serde(default)]
    pub adapter_hits: Option<[f64; 3]>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct RepoMeta {
    root: PathBuf,
}

fn write_meta(repo_dir: &Path, root: &Path) -> std::io::Result<()> {
    let path = repo_dir.join(REPO_META);
    if path.exists() {
        return Ok(());
    }
    let meta = RepoMeta {
        root: root.canonicalize().unwrap_or_else(|_| root.to_path_buf()),
    };
    fs::write(path, serde_json::to_vec(&meta)?)
}

/// Appends a query event to `repo_dir/usage.jsonl` and prunes events older than
/// [`RETENTION_DAYS`]. Does nothing when logging is off. Errors are returned, never fatal.
pub fn record_query(repo_dir: &Path, root: &Path, event: &QueryEvent) -> std::io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    fs::create_dir_all(repo_dir)?;
    write_meta(repo_dir, root)?;
    let path = repo_dir.join(QUERY_LOG);
    let mut line = serde_json::to_string(event)?;
    line.push('\n');
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?
        .write_all(line.as_bytes())?;
    prune(&path, event.ts)
}

/// Keeps only events from the last [`RETENTION_DAYS`] before `now` (rewrites the file only when
/// something is dropped).
pub fn prune(path: &Path, now: u64) -> std::io::Result<()> {
    let Ok(text) = fs::read_to_string(path) else {
        return Ok(());
    };
    let cutoff = now.saturating_sub(RETENTION_DAYS * DAY);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let kept: Vec<&str> = lines
        .iter()
        .copied()
        .filter(|l| serde_json::from_str::<QueryEvent>(l).is_ok_and(|e| e.ts >= cutoff))
        .collect();
    if kept.len() == lines.len() {
        return Ok(());
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    let tmp = path.with_extension("jsonl.tmp");
    fs::write(&tmp, out)?;
    fs::rename(tmp, path)
}

fn write_json<T: Serialize>(
    repo_dir: &Path,
    root: &Path,
    name: &str,
    v: &T,
) -> std::io::Result<()> {
    if !enabled() {
        return Ok(());
    }
    fs::create_dir_all(repo_dir)?;
    write_meta(repo_dir, root)?;
    let tmp = repo_dir.join(format!("{name}.tmp"));
    fs::write(&tmp, serde_json::to_vec(v)?)?;
    fs::rename(tmp, repo_dir.join(name))
}

/// Stores the latest index stats.
pub fn record_index(repo_dir: &Path, root: &Path, event: &IndexEvent) -> std::io::Result<()> {
    write_json(repo_dir, root, INDEX_STATS, event)
}

/// Stores the latest bench stats.
pub fn record_bench(repo_dir: &Path, root: &Path, event: &BenchEvent) -> std::io::Result<()> {
    write_json(repo_dir, root, BENCH_STATS, event)
}

/// Everything logged for one repository.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RepoUsage {
    /// Repository root, when recorded.
    pub root: Option<PathBuf>,
    /// Query events within retention.
    pub queries: Vec<QueryEvent>,
    /// Latest index stats.
    pub index: Option<IndexEvent>,
    /// Latest bench stats.
    pub bench: Option<BenchEvent>,
}

/// Reads every repository's usage under `cache_home` (events older than [`RETENTION_DAYS`]
/// before `now` are skipped). Unreadable files are ignored.
pub fn load_all(cache_home: &Path, now: u64) -> Vec<RepoUsage> {
    let cutoff = now.saturating_sub(RETENTION_DAYS * DAY);
    let Ok(dirs) = fs::read_dir(cache_home) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut paths: Vec<PathBuf> = dirs.flatten().map(|e| e.path()).collect();
    paths.sort();
    for dir in paths.into_iter().filter(|p| p.is_dir()) {
        let root = fs::read(dir.join(REPO_META))
            .ok()
            .and_then(|b| serde_json::from_slice::<RepoMeta>(&b).ok())
            .map(|m| m.root);
        let queries: Vec<QueryEvent> = fs::read_to_string(dir.join(QUERY_LOG))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str::<QueryEvent>(l).ok())
            .filter(|e| e.ts >= cutoff)
            .collect();
        let index = fs::read(dir.join(INDEX_STATS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        let bench = fs::read(dir.join(BENCH_STATS))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok());
        if root.is_none() && queries.is_empty() && index.is_none() && bench.is_none() {
            continue;
        }
        out.push(RepoUsage {
            root,
            queries,
            index,
            bench,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(ts: u64) -> QueryEvent {
        QueryEvent {
            ts,
            kind: "issue".into(),
            state: "ok".into(),
            ms: 12,
            model: "m".into(),
            adapter: true,
            files: 10,
            hinted: vec!["a.rs".into()],
        }
    }

    #[test]
    fn records_prunes_and_loads() {
        let home = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let dir = home.path().join("repo-1");
        let t = 100 * DAY;
        record_query(&dir, root.path(), &ev(t - 40 * DAY)).unwrap();
        record_query(&dir, root.path(), &ev(t)).unwrap();
        let text = fs::read_to_string(dir.join(QUERY_LOG)).unwrap();
        assert_eq!(text.lines().count(), 1, "old event pruned on write");
        record_index(&dir, root.path(), &IndexEvent::default()).unwrap();
        let all = load_all(home.path(), t);
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].queries, vec![ev(t)]);
        assert!(all[0].index.is_some());
        assert!(all[0].root.is_some());
    }
}
