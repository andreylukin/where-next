//! Read-only access to past repository states, for replaying history as a benchmark.
//!
//! Everything here uses git plumbing (`log`, `ls-tree`, `cat-file`, `rev-list`) and never
//! touches the working tree, so it is safe to run while someone else works in the repository.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use wn_sources::{kind_of, Kind};

use crate::run_git;

/// A non-merge commit with its parent, as replayed by `wn bench --history`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplayCommit {
    /// Full SHA.
    pub sha: String,
    /// First parent (the tree the commit is ranked against).
    pub parent: String,
    /// Committer date, ISO 8601.
    pub date: String,
    /// First line of the message.
    pub subject: String,
    /// Rest of the message.
    pub body: String,
    /// Paths the commit changed relative to its parent.
    pub changed: Vec<String>,
}

/// The newest `limit` non-merge commits that have a parent, oldest first.
pub fn replay_commits(root: &Path, limit: usize) -> Vec<ReplayCommit> {
    let mut commits = parse_log(root, "HEAD", limit);
    commits.reverse();
    commits
}

/// The newest `limit` non-merge commits reachable from `rev` (its ancestors, and `rev` itself),
/// newest first. Used to fit an adapter on a repository's history before a given commit.
pub fn log_from(root: &Path, rev: &str, limit: usize) -> Vec<ReplayCommit> {
    parse_log(root, rev, limit)
}

fn parse_log(root: &Path, rev: &str, limit: usize) -> Vec<ReplayCommit> {
    let count = format!("-{}", limit.max(1));
    let format = "--format=%x1e%H%x1f%P%x1f%cI%x1f%s%x1f%b%x1f";
    let Some(out) = run_git(
        root,
        &[
            "log",
            "--no-merges",
            &count,
            format,
            "--name-only",
            rev,
            "--",
        ],
    ) else {
        return Vec::new();
    };
    out.split('\u{1e}')
        .skip(1)
        .filter_map(|chunk| {
            let parts: Vec<&str> = chunk.split('\u{1f}').collect();
            if parts.len() < 6 {
                return None;
            }
            let parent = parts[1].split_whitespace().next()?.to_string();
            Some(ReplayCommit {
                sha: parts[0].to_string(),
                parent,
                date: parts[2].to_string(),
                subject: parts[3].to_string(),
                body: parts[4].to_string(),
                changed: parts[5]
                    .lines()
                    .map(str::trim)
                    .filter(|l| !l.is_empty())
                    .map(str::to_string)
                    .collect(),
            })
        })
        .collect()
}

/// `(path, blob)` of every source file (as indexed by `wn`) in the tree of `rev`, sorted by path.
pub fn source_tree(root: &Path, rev: &str) -> Vec<(String, String)> {
    let Some(out) = run_git(root, &["ls-tree", "-r", "--full-tree", rev]) else {
        return Vec::new();
    };
    let mut files: Vec<(String, String)> = out
        .lines()
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mut meta = meta.split_whitespace();
            let (_mode, kind, blob) = (meta.next()?, meta.next()?, meta.next()?);
            (kind == "blob" && kind_of(path) == Some(Kind::Source))
                .then(|| (path.to_string(), blob.to_string()))
        })
        .collect();
    files.sort();
    files
}

/// Reads blobs as text (at most `max_bytes` each, invalid UTF-8 replaced), keyed by SHA.
/// Blobs that cannot be read are missing from the result.
pub fn read_blobs(root: &Path, blobs: &[String], max_bytes: usize) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if blobs.is_empty() {
        return out;
    }
    let Ok(mut child) = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    else {
        return out;
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let list: Vec<String> = blobs.to_vec();
    // Feed from a thread: a full stdout pipe would otherwise deadlock the writer.
    let writer = std::thread::spawn(move || {
        for b in list {
            if writeln!(stdin, "{b}").is_err() {
                break;
            }
        }
    });
    let mut reader = BufReader::new(child.stdout.take().expect("piped stdout"));
    let mut header = String::new();
    for _ in 0..blobs.len() {
        header.clear();
        if reader.read_line(&mut header).unwrap_or(0) == 0 {
            break;
        }
        let fields: Vec<&str> = header.split_whitespace().collect();
        if fields.len() != 3 || fields[1] != "blob" {
            continue; // "<sha> missing" or a non-blob object: nothing follows
        }
        let Ok(size) = fields[2].parse::<usize>() else {
            break;
        };
        let mut buf = vec![0u8; size + 1]; // content plus the trailing newline
        if reader.read_exact(&mut buf).is_err() {
            break;
        }
        buf.truncate(size.min(max_bytes));
        out.insert(
            fields[0].to_string(),
            String::from_utf8_lossy(&buf).into_owned(),
        );
    }
    let _ = writer.join();
    let _ = child.wait();
    out
}

/// Whether a path is a test, fixture build output or similar (excluded from implementation-file
/// gold). Same rule as the research prototype's history replay.
pub fn is_test_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    path.contains("/test/")
        || path.contains("/tests/")
        || path.contains("/__tests__/")
        || path.starts_with("__tests__/")
        || path.contains("/dist/")
        || name.starts_with("test_")
        || name.ends_with("_test.go")
        || name.ends_with("_test.py")
        || [".spec.", ".test."].iter().any(|mid| {
            name.rfind(mid)
                .is_some_and(|i| matches!(&name[i + mid.len()..], "js" | "jsx" | "ts" | "tsx"))
        })
}

/// Ancestry among a set of commits: for each commit in `wanted`, which other `wanted` commits
/// are its ancestors. Walks at most `depth` commits of history from `HEAD`; anything reachable
/// only beyond that is treated as unrelated (the conservative answer for a leakage guard).
pub fn ancestors_among(root: &Path, wanted: &[String], depth: usize) -> Vec<Vec<bool>> {
    let n = wanted.len();
    let pos: HashMap<&str, usize> = wanted
        .iter()
        .enumerate()
        .map(|(i, s)| (s.as_str(), i))
        .collect();
    let mut out = vec![vec![false; n]; n];
    let count = format!("-{}", depth.max(1));
    let Some(list) = run_git(
        root,
        &["rev-list", "--topo-order", "--parents", &count, "HEAD"],
    ) else {
        return out;
    };
    // --topo-order lists children before parents; walk it backwards so parents come first.
    let rows: Vec<Vec<&str>> = list
        .lines()
        .map(|l| l.split_whitespace().collect())
        .filter(|r: &Vec<&str>| !r.is_empty())
        .collect();
    let words = n.div_ceil(64).max(1);
    let mut bits: HashMap<&str, Vec<u64>> = HashMap::new();
    let seen: HashSet<&str> = rows.iter().map(|r| r[0]).collect();
    for row in rows.iter().rev() {
        let mut mine = vec![0u64; words];
        for p in &row[1..] {
            if !seen.contains(p) {
                continue;
            }
            if let Some(pb) = bits.get(p) {
                for (m, x) in mine.iter_mut().zip(pb) {
                    *m |= x;
                }
            }
            if let Some(&j) = pos.get(p) {
                mine[j / 64] |= 1 << (j % 64);
            }
        }
        bits.insert(row[0], mine);
    }
    for (i, sha) in wanted.iter().enumerate() {
        if let Some(b) = bits.get(sha.as_str()) {
            for (j, cell) in out[i].iter_mut().enumerate() {
                *cell = (b[j / 64] >> (j % 64)) & 1 == 1;
            }
        }
    }
    out
}
