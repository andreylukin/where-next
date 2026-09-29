//! Repository access: which files to index, and what the history says changed together.
//!
//! This shells out to the `git` binary, like the evaluated reference, so worktrees, sparse
//! checkouts and `.gitignore` rules behave exactly as the user's own git does. Every function
//! degrades gracefully: outside a repository, [`scan`] walks the directory instead, and history
//! queries return empty results.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use wn_sources::{is_skipped, kind_of, Kind};

/// Identifies one version of a file, so unchanged files are never re-embedded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ContentId {
    /// Git blob SHA of a clean tracked file.
    Blob(String),
    /// Modification time and size of a modified, untracked or non-git file (`m<ns>-<bytes>`).
    Mtime(String),
}

impl ContentId {
    /// The id as stored in the index.
    pub fn as_str(&self) -> &str {
        match self {
            ContentId::Blob(s) | ContentId::Mtime(s) => s,
        }
    }
}

/// An indexable file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileEntry {
    /// Version id.
    pub id: ContentId,
    /// Source or config.
    pub kind: Kind,
}

/// What a scan found, so tools can say what they could not index.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Coverage {
    /// The directory is a git repository.
    pub git: bool,
    /// Tracked indexable files (clean or modified).
    pub tracked: usize,
    /// Untracked (but not ignored) indexable files.
    pub untracked: usize,
    /// Tracked indexable files with uncommitted changes.
    pub modified: usize,
    /// Tracked files missing from the working tree (dropped from the index).
    pub deleted_dropped: usize,
    /// Files of a kind where-next does not index.
    pub unsupported: usize,
    /// The most common unsupported extensions (up to 8).
    pub unsupported_ext: BTreeMap<String, usize>,
}

fn run_git(root: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The repository root containing `start`, or `start` itself outside a repository.
pub fn repo_root(start: &Path) -> PathBuf {
    match run_git(start, &["rev-parse", "--show-toplevel"]) {
        Some(out) if !out.trim().is_empty() => PathBuf::from(out.trim()),
        _ => start.to_path_buf(),
    }
    .canonicalize()
    .unwrap_or_else(|_| start.to_path_buf())
}

fn mtime_id(p: &Path) -> Option<ContentId> {
    let meta = std::fs::metadata(p).ok()?;
    let ns = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_nanos();
    Some(ContentId::Mtime(format!("m{ns}-{}", meta.len())))
}

fn ext_of(path: &str) -> String {
    let name = path.rsplit('/').next().unwrap_or(path);
    match name.rfind('.') {
        Some(i) => format!(".{}", name[i + 1..].to_lowercase()),
        None => "(none)".to_string(),
    }
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(rel) = path.strip_prefix(root) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            if !is_skipped(&format!("{rel}/")) {
                walk(root, &path, out);
            }
        } else if ft.is_file() {
            out.push(rel);
        }
    }
}

/// Indexable files in `root` (path → version and kind), plus coverage counts.
///
/// Clean tracked files use their git blob SHA; modified, untracked and non-git files use their
/// modification time and size. Files deleted from the working tree are dropped even if they are
/// still in the git index.
pub fn scan(root: &Path) -> (BTreeMap<String, FileEntry>, Coverage) {
    let mut files = BTreeMap::new();
    let mut cov = Coverage::default();
    let mut unsupported_ext: HashMap<String, usize> = HashMap::new();
    let mut unsupported = |path: &str, cov: &mut Coverage| {
        cov.unsupported += 1;
        *unsupported_ext.entry(ext_of(path)).or_default() += 1;
    };

    match run_git(root, &["ls-files", "-s", "-z"]) {
        None => {
            let mut all = Vec::new();
            walk(root, root, &mut all);
            for rel in all {
                if is_skipped(&rel) {
                    continue;
                }
                match kind_of(&rel) {
                    Some(kind) => {
                        let id = mtime_id(&root.join(&rel))
                            .unwrap_or_else(|| ContentId::Mtime(String::new()));
                        files.insert(rel, FileEntry { id, kind });
                    }
                    None => unsupported(&rel, &mut cov),
                }
            }
        }
        Some(listing) => {
            cov.git = true;
            let dirty: std::collections::HashSet<String> =
                run_git(root, &["diff", "--name-only", "-z"])
                    .unwrap_or_default()
                    .split('\0')
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
                    .collect();
            for entry in listing.split('\0') {
                let Some((meta, path)) = entry.split_once('\t') else {
                    continue;
                };
                let Some(kind) = kind_of(path) else {
                    if !is_skipped(path) {
                        unsupported(path, &mut cov);
                    }
                    continue;
                };
                let p = root.join(path);
                if !p.exists() {
                    cov.deleted_dropped += 1;
                    continue;
                }
                let id = if dirty.contains(path) {
                    cov.modified += 1;
                    mtime_id(&p).unwrap_or_else(|| ContentId::Mtime(String::new()))
                } else {
                    ContentId::Blob(meta.split_whitespace().nth(1).unwrap_or("").to_string())
                };
                cov.tracked += 1;
                files.insert(path.to_string(), FileEntry { id, kind });
            }
            let others = run_git(root, &["ls-files", "--others", "--exclude-standard", "-z"])
                .unwrap_or_default();
            for path in others.split('\0').filter(|s| !s.is_empty()) {
                let Some(kind) = kind_of(path) else {
                    if !is_skipped(path) {
                        unsupported(path, &mut cov);
                    }
                    continue;
                };
                if let Some(id) = mtime_id(&root.join(path)) {
                    cov.untracked += 1;
                    files.insert(path.to_string(), FileEntry { id, kind });
                }
            }
        }
    }
    let mut top: Vec<(String, usize)> = unsupported_ext.into_iter().collect();
    top.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    cov.unsupported_ext = top.into_iter().take(8).collect();
    (files, cov)
}

/// One non-merge commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Commit {
    /// Full SHA.
    pub sha: String,
    /// Committer date, ISO 8601.
    pub date: String,
    /// First line of the message.
    pub subject: String,
    /// Rest of the message.
    pub body: String,
    /// Paths the commit changed.
    pub paths: Vec<String>,
}

/// Newest non-merge commits reachable from `HEAD` (up to `3 × limit`, so callers can skip
/// commits whose files are no longer indexed and still reach `limit` usable ones).
pub fn history(root: &Path, limit: usize) -> Vec<Commit> {
    let count = format!("-{}", limit.saturating_mul(3).max(1));
    let format = "--format=%x1e%H%x1f%cI%x1f%s%x1f%b%x1f";
    let Some(out) = run_git(root, &["log", "--no-merges", &count, format, "--name-only"]) else {
        return Vec::new();
    };
    out.split('\u{1e}')
        .skip(1)
        .filter_map(|chunk| {
            let parts: Vec<&str> = chunk.split('\u{1f}').collect();
            if parts.len() < 5 {
                return None;
            }
            Some(Commit {
                sha: parts[0].to_string(),
                date: parts[1].to_string(),
                subject: parts[2].to_string(),
                body: parts[3].to_string(),
                paths: parts[4]
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect()
}

/// SHA of `HEAD`.
pub fn head(root: &Path) -> Option<String> {
    run_git(root, &["rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Non-merge commits on `HEAD` after `sha`, or `None` if `sha` is unknown.
pub fn commits_since(root: &Path, sha: &str) -> Option<usize> {
    if sha.is_empty() {
        return None;
    }
    run_git(
        root,
        &[
            "rev-list",
            "--count",
            "--no-merges",
            &format!("{sha}..HEAD"),
        ],
    )?
    .trim()
    .parse()
    .ok()
}

/// How often each (ordered) pair of paths changed in the same commit, skipping commits that
/// touch more than `max_files` paths (mechanical refactors make spurious pairs).
pub fn co_change(commits: &[Commit], max_files: usize) -> HashMap<(String, String), usize> {
    let mut out = HashMap::new();
    for c in commits {
        if c.paths.len() > max_files {
            continue;
        }
        let mut paths = c.paths.clone();
        paths.sort();
        paths.dedup();
        for (i, a) in paths.iter().enumerate() {
            for b in &paths[i + 1..] {
                *out.entry((a.clone(), b.clone())).or_insert(0) += 1;
            }
        }
    }
    out
}
