//! `wn bench`: measure where-next on your own repository's history, or on ContextBench.
//!
//! **History replay** (`wn bench --history`) turns past commits into tasks. For each non-merge
//! commit, the query is its message (subject plus a short first body line), the candidates are
//! the source files in its **parent** tree (the repository as it was just before the change), and
//! the gold files are the changed files that already existed there. Three rankers are compared on
//! exactly the same candidates:
//!
//! * **lexical**: BM25 over the file skeletons the model sees;
//! * **model**: the embedding model, frozen;
//! * **model + adapter**: the personal adapter, refitted every `step` commits on the `train`
//!   previous eligible commits that are git ancestors of every commit it is scored on.
//!
//! **ContextBench** (`wn bench --contextbench <dir>`) runs the same rankers on the public
//! benchmark's issues, with the adapter fitted on each repository's commits before the task's
//! base commit.
//!
//! Everything reads history through git plumbing only; the working tree is never touched.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde::{Deserialize, Serialize};
use wn_core::adapter::{
    apply_adapter, fit_adapter, sample_negatives, AdapterParams, ADAPTER_MIN_TRAIN,
};
use wn_core::encoder::{EncodeError, Encoder, QueryInput};
use wn_core::index::{EntryKind, Index};
use wn_core::machine::{step, Illegal};
use wn_core::text::history_body;
use wn_git::replay::{
    ancestors_among, is_test_path, log_from, read_blobs, replay_commits, source_tree, ReplayCommit,
};
use wn_sources::{file_doc, lang_of, MAX_SOURCE_BYTES};

// ---------------------------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------------------------

/// Stages of a benchmark run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BenchState {
    /// Not started.
    Idle,
    /// Reading history and parent trees.
    Collecting,
    /// Embedding documents and queries (resumable through the vector cache).
    Embedding,
    /// Ranking and fitting adapters.
    Scoring,
    /// Report ready.
    Done,
    /// Stopped with an error.
    Failed,
}

impl BenchState {
    /// Every state, for exhaustive tests.
    pub const ALL: [BenchState; 6] = [
        BenchState::Idle,
        BenchState::Collecting,
        BenchState::Embedding,
        BenchState::Scoring,
        BenchState::Done,
        BenchState::Failed,
    ];
}

/// Events of a benchmark run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BenchEvent {
    /// Begin.
    Start,
    /// History collected.
    Collected,
    /// Vectors ready.
    Embedded,
    /// Scores ready.
    Scored,
    /// Something failed.
    Fail,
}

impl BenchEvent {
    /// Every event, for exhaustive tests.
    pub const ALL: [BenchEvent; 5] = [
        BenchEvent::Start,
        BenchEvent::Collected,
        BenchEvent::Embedded,
        BenchEvent::Scored,
        BenchEvent::Fail,
    ];
}

/// Legal transitions; anything else is rejected and leaves the state unchanged.
pub const BENCH_TRANSITIONS: [(BenchState, BenchEvent, BenchState); 7] = [
    (BenchState::Idle, BenchEvent::Start, BenchState::Collecting),
    (
        BenchState::Collecting,
        BenchEvent::Collected,
        BenchState::Embedding,
    ),
    (
        BenchState::Embedding,
        BenchEvent::Embedded,
        BenchState::Scoring,
    ),
    (BenchState::Scoring, BenchEvent::Scored, BenchState::Done),
    (BenchState::Collecting, BenchEvent::Fail, BenchState::Failed),
    (BenchState::Embedding, BenchEvent::Fail, BenchState::Failed),
    (BenchState::Scoring, BenchEvent::Fail, BenchState::Failed),
];

/// The benchmark lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BenchLifecycle {
    state: BenchState,
}

impl Default for BenchLifecycle {
    fn default() -> Self {
        Self {
            state: BenchState::Idle,
        }
    }
}

impl BenchLifecycle {
    /// Current state.
    pub fn state(&self) -> BenchState {
        self.state
    }

    /// Applies an event.
    pub fn handle(
        &mut self,
        event: BenchEvent,
    ) -> Result<BenchState, Illegal<BenchState, BenchEvent>> {
        let next = step(&BENCH_TRANSITIONS, self.state, event)?;
        self.state = next;
        Ok(next)
    }
}

// ---------------------------------------------------------------------------------------------
// Options and report
// ---------------------------------------------------------------------------------------------

/// Benchmark options.
#[derive(Debug, Clone)]
pub struct BenchOptions {
    /// Commits to score (the newest eligible ones).
    pub commits: usize,
    /// Previous commits each adapter is fitted on.
    pub train: usize,
    /// Commits scored per adapter fit.
    pub step: usize,
    /// Skip commits changing more than this many existing files.
    pub max_gold: usize,
    /// Count test files as gold too.
    pub with_tests: bool,
    /// Fit and score the adapter.
    pub adapter: bool,
    /// Seed for negative sampling.
    pub seed: u64,
    /// Adapter hyper-parameters.
    pub params: AdapterParams,
}

impl Default for BenchOptions {
    fn default() -> Self {
        Self {
            commits: 300,
            train: 200,
            step: 100,
            max_gold: 6,
            with_tests: false,
            adapter: true,
            seed: 0,
            params: AdapterParams::default(),
        }
    }
}

/// hit@k and MRR of one ranker.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Score {
    /// Ranker.
    pub method: String,
    /// Queries scored.
    pub n: usize,
    /// Share with a gold file ranked first.
    pub hit1: f64,
    /// Share with a gold file in the top 3.
    pub hit3: f64,
    /// Share with a gold file in the top 10.
    pub hit10: f64,
    /// Mean reciprocal rank of the first gold file.
    pub mrr: f64,
}

/// hit@3 per ranker for a slice of the queries.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Slice {
    /// What the slice is.
    pub label: String,
    /// Queries in it.
    pub n: usize,
    /// Lexical hit@3.
    pub lexical: f64,
    /// Model hit@3.
    pub model: f64,
    /// Model + adapter hit@3 (on the adapted queries of the slice), if any.
    pub adapter: Option<f64>,
}

/// Where the time went.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Timing {
    /// Reading history, trees and blobs.
    pub collect_s: f64,
    /// Embedding.
    pub embed_s: f64,
    /// Ranking and adapter fits.
    pub score_s: f64,
    /// Documents embedded now.
    pub docs_embedded: usize,
    /// Documents whose vectors came from the index or the bench cache.
    pub docs_reused: usize,
}

/// A benchmark report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Report {
    /// `history` or `contextbench`.
    pub kind: String,
    /// Repository or dataset name.
    pub name: String,
    /// Model name, or `lexical fallback`.
    pub model: String,
    /// Tasks looked at (commits walked, or tasks in the file).
    pub scanned: usize,
    /// Tasks with usable gold files.
    pub eligible: usize,
    /// Tasks scored.
    pub evaluated: usize,
    /// Tasks scored with an adapter.
    pub adapted: usize,
    /// Median candidate files per task.
    pub files_median: usize,
    /// Lexical and model on every scored task.
    pub all: Vec<Score>,
    /// All three rankers on the adapted tasks (the fair comparison).
    pub matched: Vec<Score>,
    /// By candidate-set size.
    pub by_size: Vec<Slice>,
    /// By architecture era (history only).
    pub eras: Vec<Slice>,
    /// Timing.
    pub timing: Timing,
    /// Caveats.
    pub notes: Vec<String>,
}

// ---------------------------------------------------------------------------------------------
// Lexical baseline (BM25), identical to the research prototype's
// ---------------------------------------------------------------------------------------------

/// Lowercased word pieces, splitting `snake_case`, `camelCase` and paths: the regex
/// `[A-Za-z][a-z]+|[A-Z]+(?![a-z])|\d+`, implemented by hand.
pub fn tokens(text: &str) -> Vec<String> {
    let b = text.as_bytes();
    let n = b.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = b[i];
        if c.is_ascii_alphabetic() {
            if i + 1 < n && b[i + 1].is_ascii_lowercase() {
                let mut j = i + 2;
                while j < n && b[j].is_ascii_lowercase() {
                    j += 1;
                }
                out.push(text[i..j].to_ascii_lowercase());
                i = j;
                continue;
            }
            if c.is_ascii_uppercase() {
                let mut j = i + 1;
                while j < n && b[j].is_ascii_uppercase() {
                    j += 1;
                }
                // `(?![a-z])`: give back the last capital when a lowercase letter follows.
                if j < n && b[j].is_ascii_lowercase() {
                    j -= 1;
                }
                out.push(text[i..j].to_ascii_lowercase());
                i = j;
                continue;
            }
            i += 1;
        } else if c.is_ascii_digit() {
            let mut j = i + 1;
            while j < n && b[j].is_ascii_digit() {
                j += 1;
            }
            out.push(text[i..j].to_string());
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

/// Term counts of one document.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bag {
    terms: HashMap<String, u32>,
    len: u32,
}

impl Bag {
    /// Counts the tokens of `text`.
    pub fn of(text: &str) -> Bag {
        let mut terms = HashMap::new();
        let mut len = 0;
        for t in tokens(text) {
            *terms.entry(t).or_insert(0) += 1;
            len += 1;
        }
        Bag { terms, len }
    }
}

/// BM25 of `query` (tokens, repeats count) against `docs`, IDF over this candidate set.
pub fn bm25(query: &[String], docs: &[&Bag]) -> Vec<f64> {
    let (k1, b) = (1.2, 0.75);
    let n = docs.len() as f64;
    let avg = docs.iter().map(|d| f64::from(d.len)).sum::<f64>() / n.max(1.0);
    let unique: HashSet<&String> = query.iter().collect();
    let idf: HashMap<&String, f64> = unique
        .into_iter()
        .map(|t| {
            let c = docs.iter().filter(|d| d.terms.contains_key(t)).count() as f64;
            (t, (1.0 + (n - c + 0.5) / (c + 0.5)).ln())
        })
        .collect();
    docs.iter()
        .map(|d| {
            query
                .iter()
                .map(|t| match d.terms.get(t) {
                    Some(&f) => {
                        let f = f64::from(f);
                        idf[t] * f * (k1 + 1.0) / (f + k1 * (1.0 - b + b * f64::from(d.len) / avg))
                    }
                    None => 0.0,
                })
                .sum()
        })
        .collect()
}

/// 0-based rank of the best-ranked gold candidate (`None` if no gold is a candidate). Ties keep
/// candidate order, like a stable sort.
pub fn gold_rank(scores: &[f64], gold: &[usize]) -> Option<usize> {
    if gold.is_empty() {
        return None;
    }
    let mut order: Vec<usize> = (0..scores.len()).collect();
    order.sort_by(|&a, &b| {
        scores[b]
            .partial_cmp(&scores[a])
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    order.iter().position(|i| gold.contains(i))
}

/// hit@k and MRR over ranks (`None` counts as a miss).
pub fn score(method: &str, ranks: &[Option<usize>]) -> Score {
    let n = ranks.len();
    let hit = |k: usize| {
        ranks.iter().filter(|r| r.is_some_and(|r| r < k)).count() as f64 / n.max(1) as f64
    };
    let mrr = ranks
        .iter()
        .map(|r| r.map_or(0.0, |r| 1.0 / (r + 1) as f64))
        .sum::<f64>()
        / n.max(1) as f64;
    Score {
        method: method.to_string(),
        n,
        hit1: round3(hit(1)),
        hit3: round3(hit(3)),
        hit10: round3(hit(10)),
        mrr: round3(mrr),
    }
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

// ---------------------------------------------------------------------------------------------
// Shared engine: documents, vectors, rankers
// ---------------------------------------------------------------------------------------------

/// One scored task: candidates and gold as document ids.
#[derive(Debug, Clone)]
struct Task {
    sha: String,
    date: String,
    query: QueryInput,
    /// Candidate document ids, sorted by path.
    cand: Vec<usize>,
    /// Gold positions within `cand`.
    gold: Vec<usize>,
}

/// Documents keyed by `(path, blob)`, with their text and (once embedded) vectors.
#[derive(Default)]
struct Docs {
    keys: Vec<(String, String)>,
    ids: HashMap<(String, String), usize>,
    text: Vec<String>,
    vecs: Vec<Option<Vec<f32>>>,
}

impl Docs {
    fn id(&mut self, path: &str, blob: &str) -> usize {
        let key = (path.to_string(), blob.to_string());
        if let Some(&i) = self.ids.get(&key) {
            return i;
        }
        let i = self.keys.len();
        self.ids.insert(key.clone(), i);
        self.keys.push(key);
        self.text.push(String::new());
        self.vecs.push(None);
        i
    }
}

/// Keep only tasks whose complete parent-tree candidate set was read, and compact document ids.
/// Returns the original indices of the retained tasks so callers can filter parallel metadata.
fn keep_readable_tasks(
    tasks: &mut Vec<Task>,
    docs: &mut Docs,
    texts: &HashMap<String, String>,
) -> Vec<usize> {
    let old = std::mem::take(docs);
    let mut kept = Vec::new();
    let mut original = 0;
    tasks.retain_mut(|task| {
        let readable = task
            .cand
            .iter()
            .all(|&id| texts.contains_key(&old.keys[id].1));
        if readable {
            kept.push(original);
        }
        original += 1;
        readable
    });
    let mut compact = Docs::default();
    for task in tasks {
        for id in &mut task.cand {
            let (path, blob) = &old.keys[*id];
            let new_id = compact.id(path, blob);
            compact.text[new_id] = file_doc(path, &texts[blob]);
            *id = new_id;
        }
    }
    *docs = compact;
    kept
}

/// Fetch missing promisor blobs in one request before reading the candidate texts.
fn load_blobs(root: &Path, blobs: &[String], log: &mut dyn FnMut(&str)) -> HashMap<String, String> {
    if blobs.is_empty() {
        return HashMap::new();
    }
    let check = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["cat-file", "--batch-check"])
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .ok();
    if let Some(mut child) = check {
        let mut stdin = child.stdin.take().expect("piped stdin");
        let input = blobs.join("\n");
        let writer = std::thread::spawn(move || writeln!(stdin, "{input}"));
        let output = child.wait_with_output();
        let _ = writer.join();
        if let Ok(output) = output {
            if output.status.success() {
                let missing: Vec<&str> = std::str::from_utf8(&output.stdout)
                    .unwrap_or("")
                    .lines()
                    .filter_map(|line| line.strip_suffix(" missing"))
                    .collect();
                if !missing.is_empty() {
                    log(&format!(
                        "fetching {} missing historical blobs",
                        missing.len()
                    ));
                    let remote = Command::new("git")
                        .arg("-C")
                        .arg(root)
                        .args(["config", "--get-regexp", r"^remote\..*\.promisor$"])
                        .output()
                        .ok()
                        .filter(|output| output.status.success())
                        .and_then(|output| {
                            String::from_utf8(output.stdout).ok().and_then(|text| {
                                text.lines().find_map(|line| {
                                    let (key, value) = line.split_once(' ')?;
                                    (value == "true")
                                        .then(|| {
                                            key.strip_prefix("remote.")?.strip_suffix(".promisor")
                                        })
                                        .flatten()
                                        .map(str::to_owned)
                                })
                            })
                        })
                        .unwrap_or_else(|| "origin".to_string());
                    if let Ok(mut fetch) = Command::new("git")
                        .arg("-C")
                        .arg(root)
                        .args([
                            "-c",
                            "fetch.negotiationAlgorithm=noop",
                            "fetch",
                            &remote,
                            "--no-tags",
                            "--no-write-fetch-head",
                            "--recurse-submodules=no",
                            "--filter=blob:none",
                            "--stdin",
                        ])
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    {
                        if let Some(mut stdin) = fetch.stdin.take() {
                            let _ = writeln!(stdin, "{}", missing.join("\n"));
                        }
                        if !fetch.wait().is_ok_and(|status| status.success()) {
                            log("historical blob fetch failed; unreadable commits will be skipped");
                        }
                    }
                }
            }
        }
    }
    read_blobs(root, blobs, MAX_SOURCE_BYTES)
}

/// Vectors saved between runs, keyed by `path\tblob` (a rerun only embeds new file versions).
#[derive(Serialize, Deserialize)]
struct CacheMeta {
    version: u8,
    fingerprint: String,
    dim: usize,
    keys: Vec<String>,
}

fn cache_load(dir: &Path, fingerprint: &str) -> HashMap<String, Vec<f32>> {
    let mut out = HashMap::new();
    let (Ok(meta), Ok(bytes)) = (
        std::fs::read(dir.join("bench-vectors.json")),
        std::fs::read(dir.join("bench-vectors.bin")),
    ) else {
        return out;
    };
    let Ok(meta) = serde_json::from_slice::<CacheMeta>(&meta) else {
        return out;
    };
    if meta.version != 1
        || meta.fingerprint != fingerprint
        || bytes.len() != meta.keys.len() * meta.dim * 4
    {
        return out;
    }
    for (k, chunk) in meta.keys.into_iter().zip(bytes.chunks_exact(meta.dim * 4)) {
        let v = chunk
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        out.insert(k, v);
    }
    out
}

fn cache_save(dir: &Path, fingerprint: &str, docs: &Docs) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let mut keys = Vec::new();
    let mut bytes = Vec::new();
    let mut dim = 0;
    for (k, v) in docs.keys.iter().zip(&docs.vecs) {
        if let Some(v) = v {
            dim = v.len();
            keys.push(format!("{}\t{}", k.0, k.1));
            for x in v {
                bytes.extend_from_slice(&x.to_le_bytes());
            }
        }
    }
    let meta = CacheMeta {
        version: 1,
        fingerprint: fingerprint.to_string(),
        dim,
        keys,
    };
    let tmp_bin = dir.join("bench-vectors.bin.tmp");
    let tmp_meta = dir.join("bench-vectors.json.tmp");
    std::fs::write(&tmp_bin, &bytes)?;
    std::fs::write(&tmp_meta, serde_json::to_vec(&meta)?)?;
    std::fs::rename(tmp_bin, dir.join("bench-vectors.bin"))?;
    std::fs::rename(tmp_meta, dir.join("bench-vectors.json"))?;
    Ok(())
}

/// Fills in document texts (from git blobs) and vectors (index, cache, then the encoder).
#[allow(clippy::too_many_arguments)]
fn embed_docs(
    docs: &mut Docs,
    encoder: &dyn Encoder,
    reuse: Option<&Index>,
    cache_dir: &Path,
    timing: &mut Timing,
    log: &mut dyn FnMut(&str),
) -> Result<(), EncodeError> {
    let fingerprint = encoder.fingerprint();
    let mut known = cache_load(cache_dir, &fingerprint);
    if let Some(index) = reuse.filter(|ix| ix.fingerprint() == fingerprint) {
        let (rows, mat) = index.matrix(EntryKind::File);
        let d = index.dim();
        for (k, e) in rows.iter().enumerate() {
            known
                .entry(format!("{}\t{}", e.path, e.cid))
                .or_insert_with(|| mat[k * d..(k + 1) * d].to_vec());
        }
    }
    let mut missing = Vec::new();
    for (i, (path, blob)) in docs.keys.iter().enumerate() {
        match known.get(&format!("{path}\t{blob}")) {
            Some(v) => docs.vecs[i] = Some(v.clone()),
            None => missing.push(i),
        }
    }
    timing.docs_reused = docs.keys.len() - missing.len();
    timing.docs_embedded = missing.len();
    let total = missing.len();
    let mut since_save = 0;
    for (done, chunk) in missing.chunks(256).enumerate() {
        let texts: Vec<String> = chunk.iter().map(|&i| docs.text[i].clone()).collect();
        let vecs = encoder.documents(&texts)?;
        for (&i, v) in chunk.iter().zip(vecs) {
            docs.vecs[i] = Some(v);
        }
        since_save += chunk.len();
        let n = ((done + 1) * 256).min(total);
        log(&format!("embedding documents {n}/{total}"));
        if since_save >= 2048 {
            let _ = cache_save(cache_dir, &fingerprint, docs);
            since_save = 0;
        }
    }
    if total > 0 {
        let _ = cache_save(cache_dir, &fingerprint, docs);
    }
    Ok(())
}

fn dot(a: &[f32], b: &[f32]) -> f64 {
    f64::from(a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>())
}

/// Ranks of one query vector over each task's candidates.
fn model_rank(q: &[f32], task: &Task, docs: &Docs) -> Option<usize> {
    let scores: Vec<f64> = task
        .cand
        .iter()
        .map(|&c| dot(q, docs.vecs[c].as_deref().unwrap_or(&[])))
        .collect();
    gold_rank(&scores, &task.gold)
}

fn lexical_rank(task: &Task, bags: &[Bag]) -> Option<usize> {
    let body = task.query.query.as_str();
    let q = tokens(body);
    let cands: Vec<&Bag> = task.cand.iter().map(|&c| &bags[c]).collect();
    gold_rank(&bm25(&q, &cands), &task.gold)
}

fn median(mut v: Vec<usize>) -> usize {
    if v.is_empty() {
        return 0;
    }
    v.sort_unstable();
    v[v.len() / 2]
}

fn size_label(n: usize) -> &'static str {
    match n {
        0..=999 => "< 1k files",
        1000..=2999 => "1k-3k files",
        3000..=9999 => "3k-10k files",
        _ => "10k+ files",
    }
}

fn hit3(ranks: &[Option<usize>]) -> f64 {
    score("", ranks).hit3
}

/// Slices of the scored tasks by a label.
fn slices(
    labels: &[String],
    lex: &[Option<usize>],
    model: &[Option<usize>],
    adapted: &[Option<Option<usize>>],
) -> Vec<Slice> {
    let mut order: Vec<String> = Vec::new();
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, l) in labels.iter().enumerate() {
        if !groups.contains_key(l) {
            order.push(l.clone());
        }
        groups.entry(l.clone()).or_default().push(i);
    }
    order
        .into_iter()
        .map(|l| {
            let ix = &groups[&l];
            let pick = |v: &[Option<usize>]| ix.iter().map(|&i| v[i]).collect::<Vec<_>>();
            let ad: Vec<Option<usize>> = ix.iter().filter_map(|&i| adapted[i]).collect();
            Slice {
                label: l,
                n: ix.len(),
                lexical: hit3(&pick(lex)),
                model: hit3(&pick(model)),
                adapter: (!ad.is_empty()).then(|| hit3(&ad)),
            }
        })
        .collect()
}

/// Fits one adapter on `examples` (task index, positive doc id, negative doc ids) and returns W.
fn fit_on(
    examples: &[(usize, usize, Vec<usize>)],
    qvecs: &[Vec<f32>],
    mat: &[f32],
    d: usize,
    params: &AdapterParams,
) -> Vec<f32> {
    let qv: Vec<f32> = examples
        .iter()
        .flat_map(|(t, _, _)| qvecs[*t].iter().copied())
        .collect();
    let pos: Vec<usize> = examples.iter().map(|(_, p, _)| *p).collect();
    let negs: Vec<Vec<usize>> = examples.iter().map(|(_, _, n)| n.clone()).collect();
    fit_adapter(&qv, mat, d, &pos, &negs, params)
}

/// Samples up to `k` of `pool` without replacement (seeded, reproducible).
fn sample_from(pool: &[usize], k: usize, seed: u64) -> Vec<usize> {
    let picks = sample_negatives(&[usize::MAX], pool.len(), k, seed);
    picks[0].iter().map(|&j| pool[j]).collect()
}

// ---------------------------------------------------------------------------------------------
// History replay
// ---------------------------------------------------------------------------------------------

/// Replays the newest eligible commits of the repository at `root`.
pub fn history(
    root: &Path,
    encoder: &dyn Encoder,
    model_name: &str,
    reuse: Option<&Index>,
    cache_dir: &Path,
    opts: &BenchOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Report, String> {
    let mut life = BenchLifecycle::default();
    let fail = |life: &mut BenchLifecycle, e: String| {
        let _ = life.handle(BenchEvent::Fail);
        e
    };
    let _ = life.handle(BenchEvent::Start);
    let t0 = std::time::Instant::now();
    let want = opts.commits + if opts.adapter { opts.train } else { 0 };
    let walked = replay_commits(root, want.saturating_mul(4).max(50));
    if walked.is_empty() {
        return Err(fail(
            &mut life,
            "no commits with a parent to replay (is this a git repository?)".into(),
        ));
    }
    log(&format!("reading {} commits", walked.len()));
    // Newest first: collect eligible commits until we have enough.
    let mut picked: Vec<(ReplayCommit, Vec<String>)> = Vec::new();
    let mut trees: HashMap<String, Vec<(String, String)>> = HashMap::new();
    let mut scanned = 0;
    let candidates: Vec<&ReplayCommit> = walked
        .iter()
        .rev()
        .filter(|c| {
            c.changed
                .iter()
                .any(|p| wn_sources::kind_of(p) == Some(wn_sources::Kind::Source))
        })
        .collect();
    for chunk in candidates.chunks(64) {
        let need: Vec<&str> = chunk
            .iter()
            .map(|c| c.parent.as_str())
            .filter(|p| !trees.contains_key(*p))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let got: Vec<(String, Vec<(String, String)>)> = std::thread::scope(|s| {
            let handles: Vec<_> = need
                .chunks(need.len().div_ceil(8).max(1))
                .map(|part| {
                    s.spawn(move || {
                        part.iter()
                            .map(|p| (p.to_string(), source_tree(root, p)))
                            .collect::<Vec<_>>()
                    })
                })
                .collect();
            handles
                .into_iter()
                .flat_map(|h| h.join().unwrap_or_default())
                .collect()
        });
        trees.extend(got);
        for c in chunk {
            scanned += 1;
            let tree = &trees[&c.parent];
            let present: HashSet<&str> = tree.iter().map(|(p, _)| p.as_str()).collect();
            let mut full: Vec<String> = c
                .changed
                .iter()
                .filter(|p| present.contains(p.as_str()))
                .cloned()
                .collect();
            full.sort();
            full.dedup();
            let gold: Vec<String> = full
                .iter()
                .filter(|p| opts.with_tests || !is_test_path(p))
                .cloned()
                .collect();
            if full.is_empty() || full.len() > opts.max_gold || gold.is_empty() {
                continue;
            }
            picked.push(((*c).clone(), gold));
            if picked.len() >= want {
                break;
            }
        }
        if picked.len() >= want {
            break;
        }
    }
    picked.reverse(); // oldest first
    if picked.is_empty() {
        return Err(fail(
            &mut life,
            "no commits changed an existing source file".into(),
        ));
    }
    let mut docs = Docs::default();
    let mut tasks = Vec::new();
    let mut langs_of: Vec<String> = Vec::new();
    let mut dirs_of: Vec<HashSet<String>> = Vec::new();
    for (c, gold) in &picked {
        let tree = &trees[&c.parent];
        let cand: Vec<usize> = tree.iter().map(|(p, b)| docs.id(p, b)).collect();
        let gold_pos: Vec<usize> = tree
            .iter()
            .enumerate()
            .filter(|(_, (p, _))| gold.binary_search(p).is_ok())
            .map(|(k, _)| k)
            .collect();
        let mut langs: HashMap<&str, usize> = HashMap::new();
        let mut dirs = HashSet::new();
        for (p, _) in tree {
            if let Some(l) = lang_of(p) {
                *langs.entry(l).or_insert(0) += 1;
            }
            let mut parts = p.splitn(3, '/');
            let head = match (parts.next(), parts.next(), parts.next()) {
                (Some(a), Some(b), Some(_)) => format!("{a}/{b}"),
                _ => p.clone(),
            };
            dirs.insert(head);
        }
        let dominant = langs
            .iter()
            .max_by(|a, b| a.1.cmp(b.1).then(b.0.cmp(a.0)))
            .map_or("", |(l, _)| *l);
        langs_of.push(dominant.to_string());
        dirs_of.push(dirs);
        tasks.push(Task {
            sha: c.sha.clone(),
            date: c.date.clone(),
            query: QueryInput::file(history_body(&c.subject, &c.body)),
            cand,
            gold: gold_pos,
        });
    }
    let mut blobs: Vec<String> = docs.keys.iter().map(|(_, blob)| blob.clone()).collect();
    blobs.sort_unstable();
    blobs.dedup();
    let texts = load_blobs(root, &blobs, log);
    let kept = keep_readable_tasks(&mut tasks, &mut docs, &texts);
    let unreadable = picked.len() - kept.len();
    langs_of = kept.iter().map(|&i| langs_of[i].clone()).collect();
    dirs_of = kept.iter().map(|&i| dirs_of[i].clone()).collect();
    if tasks.is_empty() {
        return Err(fail(
            &mut life,
            format!("no commits have readable candidate blobs ({unreadable} skipped)"),
        ));
    }
    let mut timing = Timing::default();
    let _ = life.handle(BenchEvent::Collected);
    log(&format!(
        "{} eligible commits, {} distinct file versions",
        tasks.len(),
        docs.keys.len()
    ));
    let t1 = std::time::Instant::now();
    embed_docs(&mut docs, encoder, reuse, cache_dir, &mut timing, log)
        .map_err(|e| fail(&mut life, format!("embedding failed: {e}")))?;
    let queries: Vec<QueryInput> = tasks.iter().map(|t| t.query.clone()).collect();
    let qvecs = encoder
        .queries(&queries)
        .map_err(|e| fail(&mut life, format!("embedding failed: {e}")))?;
    timing.collect_s = round1(t1.duration_since(t0).as_secs_f64());
    timing.embed_s = round1(t1.elapsed().as_secs_f64());
    if let Some(path) = std::env::var_os("WN_BENCH_DUMP") {
        // Debugging aid for parity checks against the research prototype: the vectors used.
        let docs_json: serde_json::Map<String, serde_json::Value> = docs
            .keys
            .iter()
            .zip(&docs.vecs)
            .map(|(k, v)| (format!("{}\t{}", k.0, k.1), serde_json::json!(v)))
            .collect();
        let q_json: serde_json::Map<String, serde_json::Value> = tasks
            .iter()
            .zip(&qvecs)
            .map(|(t, v)| (t.sha.clone(), serde_json::json!(v)))
            .collect();
        let dump = serde_json::json!({ "docs": docs_json, "queries": q_json });
        let _ = std::fs::write(path, dump.to_string());
    }
    let _ = life.handle(BenchEvent::Embedded);
    let t2 = std::time::Instant::now();

    let eval_from = tasks.len().saturating_sub(opts.commits);
    let bags: Vec<Bag> = docs.text.iter().map(|t| Bag::of(t)).collect();
    let mut lex = Vec::new();
    let mut model = Vec::new();
    for t in &tasks[eval_from..] {
        lex.push(lexical_rank(t, &bags));
    }
    for (i, t) in tasks.iter().enumerate().skip(eval_from) {
        model.push(model_rank(&qvecs[i], t, &docs));
    }
    // Rolling adapter, fitted only on ancestors of every commit it scores.
    let mut adapted: Vec<Option<Option<usize>>> = vec![None; tasks.len() - eval_from];
    let mut notes = Vec::new();
    if unreadable > 0 {
        notes.push(format!(
            "{unreadable} commits skipped because candidate blobs could not be read"
        ));
    }
    if opts.adapter {
        let d = qvecs.first().map_or(0, Vec::len);
        let mat: Vec<f32> = docs
            .vecs
            .iter()
            .flat_map(|v| v.as_deref().unwrap_or(&[]).iter().copied())
            .collect();
        let shas: Vec<String> = tasks.iter().map(|t| t.sha.clone()).collect();
        let depth = (tasks.len() * 20).max(5000);
        let anc = ancestors_among(root, &shas, depth);
        let mut blocks = 0;
        // Short histories: start once enough earlier commits exist, and refit more often so
        // every block is fitted on most of what came before it.
        let mut start = eval_from.max(ADAPTER_MIN_TRAIN.min(tasks.len()));
        let step_len = opts.step.min(start.max(10));
        while start < tasks.len() {
            let end = (start + step_len).min(tasks.len());
            let allowed: Vec<usize> = (0..start)
                .filter(|&j| (start..end).all(|b| anc[b][j]))
                .collect();
            let train: Vec<usize> = allowed[allowed.len().saturating_sub(opts.train)..].to_vec();
            if train.len() >= ADAPTER_MIN_TRAIN && d > 0 {
                let examples: Vec<(usize, usize, Vec<usize>)> = train
                    .iter()
                    .map(|&j| {
                        let t = &tasks[j];
                        let pos = t.cand[t.gold[0]];
                        let others: Vec<usize> = t
                            .cand
                            .iter()
                            .enumerate()
                            .filter(|(k, _)| !t.gold.contains(k))
                            .map(|(_, &c)| c)
                            .collect();
                        let seed = opts.seed ^ (j as u64).wrapping_mul(0x9E37_79B9);
                        (j, pos, sample_from(&others, opts.params.negatives, seed))
                    })
                    .collect();
                let w = fit_on(&examples, &qvecs, &mat, d, &opts.params);
                for b in start..end {
                    let q = apply_adapter(&qvecs[b], &w, d);
                    adapted[b - eval_from] = Some(model_rank(&q, &tasks[b], &docs));
                }
                blocks += 1;
            }
            start = end;
        }
        log(&format!("fitted {blocks} adapters"));
        let skipped = adapted.iter().filter(|a| a.is_none()).count();
        if skipped > 0 {
            notes.push(format!(
                "{skipped} scored commits had fewer than {ADAPTER_MIN_TRAIN} earlier ancestor commits to fit an adapter on; the adapter rows cover the rest"
            ));
        }
    }
    let n_eval = tasks.len() - eval_from;
    let all = vec![score("lexical (BM25)", &lex), score("model", &model)];
    let adapted_ix: Vec<usize> = (0..n_eval).filter(|&i| adapted[i].is_some()).collect();
    let matched = if adapted_ix.is_empty() {
        Vec::new()
    } else {
        let pick = |v: &[Option<usize>]| adapted_ix.iter().map(|&i| v[i]).collect::<Vec<_>>();
        let ad: Vec<Option<usize>> = adapted_ix.iter().map(|&i| adapted[i].flatten()).collect();
        vec![
            score("lexical (BM25)", &pick(&lex)),
            score("model", &pick(&model)),
            score("model + adapter", &ad),
        ]
    };
    let sizes: Vec<String> = tasks[eval_from..]
        .iter()
        .map(|t| size_label(t.cand.len()).to_string())
        .collect();
    let mut by_size = slices(&sizes, &lex, &model, &adapted);
    by_size.sort_by_key(|s| {
        ["< 1k files", "1k-3k files", "3k-10k files", "10k+ files"]
            .iter()
            .position(|l| *l == s.label)
    });
    let era_starts = eras(&dirs_of[eval_from..], &langs_of[eval_from..], 0.4, 40);
    let mut era_labels = vec![String::new(); n_eval];
    for (e, &s) in era_starts.iter().enumerate() {
        let end = era_starts.get(e + 1).copied().unwrap_or(n_eval);
        let from = &tasks[eval_from + s].date;
        let to = &tasks[eval_from + end - 1].date;
        let lang = &langs_of[eval_from + s];
        let label = format!(
            "{} to {} ({lang})",
            &from[..from.len().min(10)],
            &to[..to.len().min(10)]
        );
        for l in &mut era_labels[s..end] {
            l.clone_from(&label);
        }
    }
    let eras_out = slices(&era_labels, &lex, &model, &adapted);
    timing.score_s = round1(t2.elapsed().as_secs_f64());
    let _ = life.handle(BenchEvent::Scored);
    if encoder.fingerprint().starts_with("hash-") {
        notes
            .push("no model installed: the \"model\" rows use the lexical fallback encoder".into());
    }
    Ok(Report {
        kind: "history".into(),
        name: root.file_name().map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        model: model_name.to_string(),
        scanned,
        eligible: tasks.len(),
        evaluated: n_eval,
        adapted: adapted_ix.len(),
        files_median: median(tasks[eval_from..].iter().map(|t| t.cand.len()).collect()),
        all,
        matched,
        by_size,
        eras: eras_out,
        timing,
        notes,
    })
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Start indices of architecture eras: a new era begins where the set of top-level directories
/// drops below `threshold` Jaccard similarity to the era's first commit, or the dominant language
/// changes; eras shorter than `min_size` commits are merged into the previous one.
pub fn eras(
    dirs: &[HashSet<String>],
    dominant: &[String],
    threshold: f64,
    min_size: usize,
) -> Vec<usize> {
    if dirs.is_empty() {
        return Vec::new();
    }
    let mut starts = vec![0];
    for i in 1..dirs.len() {
        let r = *starts.last().expect("non-empty");
        let union = dirs[r].union(&dirs[i]).count().max(1);
        let inter = dirs[r].intersection(&dirs[i]).count();
        if (inter as f64) / (union as f64) < threshold || dominant[i] != dominant[r] {
            starts.push(i);
        }
    }
    let mut merged = vec![starts[0]];
    for (k, &s) in starts.iter().enumerate().skip(1) {
        let next = starts.get(k + 1).copied().unwrap_or(dirs.len());
        if next - s >= min_size {
            merged.push(s);
        }
    }
    merged
}

// ---------------------------------------------------------------------------------------------
// ContextBench
// ---------------------------------------------------------------------------------------------

/// One ContextBench task, as prepared for `wn bench --contextbench` (one JSON object per line
/// of `tasks.jsonl`).
#[derive(Debug, Clone, Deserialize)]
pub struct CbTask {
    /// Task id.
    pub instance_id: String,
    /// Local clone of the repository (relative to `tasks.jsonl`, or absolute).
    pub repo_dir: String,
    /// Commit before the fix.
    pub base_commit: String,
    /// Issue text.
    pub problem_statement: String,
    /// Gold file paths (a `/workspace/<name>/` prefix is stripped).
    pub gold_files: Vec<String>,
}

/// Normalises a ContextBench gold path to be repository-relative.
pub fn repo_relative(path: &str) -> String {
    let p = path.strip_prefix("/workspace/").map_or(path, |rest| {
        rest.split_once('/').map_or(rest, |(_, tail)| tail)
    });
    p.trim_start_matches("./")
        .trim_start_matches('/')
        .to_string()
}

/// Runs ContextBench tasks listed in `dir/tasks.jsonl`.
pub fn contextbench(
    dir: &Path,
    encoder: &dyn Encoder,
    model_name: &str,
    cache_root: &dyn Fn(&Path) -> PathBuf,
    opts: &BenchOptions,
    log: &mut dyn FnMut(&str),
) -> Result<Report, String> {
    let file = dir.join("tasks.jsonl");
    let text = std::fs::read_to_string(&file).map_err(|e| {
        format!(
            "cannot read {}: {e} (see benchmarks/contextbench.md)",
            file.display()
        )
    })?;
    let all_tasks: Vec<CbTask> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect::<Result<_, _>>()
        .map_err(|e| format!("bad tasks.jsonl: {e}"))?;
    let mut life = BenchLifecycle::default();
    let _ = life.handle(BenchEvent::Start);
    let mut by_repo: BTreeMap<String, Vec<&CbTask>> = BTreeMap::new();
    for t in &all_tasks {
        by_repo.entry(t.repo_dir.clone()).or_default().push(t);
    }
    let mut lex = Vec::new();
    let mut model = Vec::new();
    let mut adapted: Vec<Option<Option<usize>>> = Vec::new();
    let mut sizes = Vec::new();
    let mut timing = Timing::default();
    let mut eligible = 0;
    let mut notes = Vec::new();
    let mut missing_repos = 0;
    for (repo, tasks) in &by_repo {
        let root = if Path::new(repo).is_absolute() {
            PathBuf::from(repo)
        } else {
            dir.join(repo)
        };
        if !root.join(".git").exists() && !root.join("HEAD").exists() {
            missing_repos += 1;
            continue;
        }
        log(&format!("{repo}: {} tasks", tasks.len()));
        let mut docs = Docs::default();
        let mut prepared = Vec::new();
        for t in tasks {
            let tree = source_tree(&root, &t.base_commit);
            if tree.is_empty() {
                continue;
            }
            let gold: HashSet<String> = t.gold_files.iter().map(|g| repo_relative(g)).collect();
            let cand: Vec<usize> = tree.iter().map(|(p, b)| docs.id(p, b)).collect();
            let gold_pos: Vec<usize> = tree
                .iter()
                .enumerate()
                .filter(|(_, (p, _))| gold.contains(p))
                .map(|(k, _)| k)
                .collect();
            eligible += 1;
            prepared.push((
                t,
                Task {
                    sha: t.base_commit.clone(),
                    date: String::new(),
                    query: QueryInput::file(t.problem_statement.clone()),
                    cand,
                    gold: gold_pos,
                },
            ));
        }
        let mut blobs: Vec<String> = docs.keys.iter().map(|(_, blob)| blob.clone()).collect();
        blobs.sort_unstable();
        blobs.dedup();
        let texts = load_blobs(&root, &blobs, log);
        let mut ready: Vec<Task> = prepared.iter().map(|(_, task)| task.clone()).collect();
        let kept = keep_readable_tasks(&mut ready, &mut docs, &texts);
        let unreadable = prepared.len() - kept.len();
        eligible -= unreadable;
        if unreadable > 0 {
            notes.push(format!(
                "{repo}: {unreadable} tasks skipped because candidate blobs could not be read"
            ));
        }
        prepared = kept
            .into_iter()
            .zip(ready)
            .map(|(i, task)| (prepared[i].0, task))
            .collect();
        if prepared.is_empty() {
            continue;
        }
        let t0 = std::time::Instant::now();
        let mut part = Timing::default();
        embed_docs(&mut docs, encoder, None, &cache_root(&root), &mut part, log)
            .map_err(|e| format!("embedding failed: {e}"))?;
        timing.docs_embedded += part.docs_embedded;
        timing.docs_reused += part.docs_reused;
        timing.embed_s += t0.elapsed().as_secs_f64();
        let t1 = std::time::Instant::now();
        let bags: Vec<Bag> = docs.text.iter().map(|t| Bag::of(t)).collect();
        let queries: Vec<QueryInput> = prepared.iter().map(|(_, t)| t.query.clone()).collect();
        let qvecs = encoder
            .queries(&queries)
            .map_err(|e| format!("embedding failed: {e}"))?;
        let d = qvecs.first().map_or(0, Vec::len);
        let mat: Vec<f32> = docs
            .vecs
            .iter()
            .flat_map(|v| v.as_deref().unwrap_or(&[]).iter().copied())
            .collect();
        for (k, (cb, task)) in prepared.iter().enumerate() {
            lex.push(lexical_rank(task, &bags));
            model.push(model_rank(&qvecs[k], task, &docs));
            sizes.push(size_label(task.cand.len()).to_string());
            let mut result = None;
            if opts.adapter && d > 0 {
                // Adapter on the repository's commits before the base commit (ancestors only),
                // with the base tree as candidates: the first changed file present is the positive.
                let history = log_from(&root, &cb.base_commit, opts.train);
                let position: HashMap<usize, usize> =
                    task.cand.iter().enumerate().map(|(k, &c)| (c, k)).collect();
                let path_id: HashMap<&str, usize> = task
                    .cand
                    .iter()
                    .map(|&c| (docs.keys[c].0.as_str(), c))
                    .collect();
                let mut hq = Vec::new();
                let mut pos = Vec::new();
                for h in &history {
                    if let Some(&id) = h.changed.iter().find_map(|p| path_id.get(p.as_str())) {
                        hq.push(QueryInput::file(history_body(&h.subject, &h.body)));
                        pos.push(position[&id]);
                    }
                }
                if hq.len() >= ADAPTER_MIN_TRAIN {
                    let hv: Vec<f32> = encoder
                        .queries(&hq)
                        .map_err(|e| format!("embedding failed: {e}"))?
                        .into_iter()
                        .flatten()
                        .collect();
                    let cand_mat: Vec<f32> = task
                        .cand
                        .iter()
                        .flat_map(|&c| mat[c * d..(c + 1) * d].iter().copied())
                        .collect();
                    let negs =
                        sample_negatives(&pos, task.cand.len(), opts.params.negatives, opts.seed);
                    let w = fit_adapter(&hv, &cand_mat, d, &pos, &negs, &opts.params);
                    let q = apply_adapter(&qvecs[k], &w, d);
                    result = Some(model_rank(&q, task, &docs));
                }
            }
            adapted.push(result);
        }
        timing.score_s += t1.elapsed().as_secs_f64();
    }
    if missing_repos > 0 {
        notes.push(format!(
            "{missing_repos} repositories were not found locally and were skipped"
        ));
    }
    let _ = life.handle(BenchEvent::Collected);
    let _ = life.handle(BenchEvent::Embedded);
    let _ = life.handle(BenchEvent::Scored);
    timing.embed_s = round1(timing.embed_s);
    timing.score_s = round1(timing.score_s);
    let n = lex.len();
    let adapted_ix: Vec<usize> = (0..n).filter(|&i| adapted[i].is_some()).collect();
    let matched = if adapted_ix.is_empty() {
        Vec::new()
    } else {
        let pick = |v: &[Option<usize>]| adapted_ix.iter().map(|&i| v[i]).collect::<Vec<_>>();
        let ad: Vec<Option<usize>> = adapted_ix.iter().map(|&i| adapted[i].flatten()).collect();
        vec![
            score("lexical (BM25)", &pick(&lex)),
            score("model", &pick(&model)),
            score("model + adapter", &ad),
        ]
    };
    let mut by_size = slices(&sizes, &lex, &model, &adapted);
    by_size.sort_by_key(|s| {
        ["< 1k files", "1k-3k files", "3k-10k files", "10k+ files"]
            .iter()
            .position(|l| *l == s.label)
    });
    Ok(Report {
        kind: "contextbench".into(),
        name: dir.file_name().map_or_else(
            || dir.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        ),
        model: model_name.to_string(),
        scanned: all_tasks.len(),
        eligible,
        evaluated: n,
        adapted: adapted_ix.len(),
        files_median: 0,
        all: vec![score("lexical (BM25)", &lex), score("model", &model)],
        matched,
        by_size,
        eras: Vec::new(),
        timing,
        notes,
    })
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

fn table(out: &mut String, rows: &[Score]) {
    let _ = writeln!(
        out,
        "  {:<18} {:>6} {:>6} {:>6} {:>6} {:>6}",
        "", "hit@1", "hit@3", "hit@10", "MRR", "n"
    );
    for r in rows {
        let _ = writeln!(
            out,
            "  {:<18} {:>6.3} {:>6.3} {:>6.3} {:>6.3} {:>6}",
            r.method, r.hit1, r.hit3, r.hit10, r.mrr, r.n
        );
    }
}

fn slice_table(out: &mut String, title: &str, rows: &[Slice]) {
    if rows.is_empty() {
        return;
    }
    let _ = writeln!(out, "\n{title} (hit@3)");
    let _ = writeln!(
        out,
        "  {:<34} {:>5} {:>8} {:>6} {:>8}",
        "", "n", "lexical", "model", "+adapter"
    );
    for s in rows {
        let ad = s.adapter.map_or("-".to_string(), |a| format!("{a:.3}"));
        let _ = writeln!(
            out,
            "  {:<34} {:>5} {:>8.3} {:>6.3} {:>8}",
            s.label, s.n, s.lexical, s.model, ad
        );
    }
}

use std::fmt::Write as _;

/// Human-readable report.
pub fn render(r: &Report) -> String {
    let mut out = String::new();
    let what = if r.kind == "history" {
        "history replay"
    } else {
        "ContextBench"
    };
    let _ = writeln!(out, "where-next {what}: {}", r.name);
    let _ = writeln!(out, "model: {}", r.model);
    if r.kind == "history" {
        let _ = writeln!(
            out,
            "commits: {} scanned, {} eligible, {} scored ({} with an adapter); median {} files per commit",
            r.scanned, r.eligible, r.evaluated, r.adapted, r.files_median
        );
    } else {
        let _ = writeln!(
            out,
            "tasks: {} listed, {} runnable, {} scored ({} with an adapter)",
            r.scanned, r.eligible, r.evaluated, r.adapted
        );
    }
    if !r.matched.is_empty() {
        let _ = writeln!(
            out,
            "\nwith the personal adapter (same {} tasks)",
            r.adapted
        );
        table(&mut out, &r.matched);
    }
    if r.matched.is_empty() || r.adapted < r.evaluated {
        let _ = writeln!(out, "\nall scored tasks");
        table(&mut out, &r.all);
    }
    slice_table(&mut out, "by repository size", &r.by_size);
    if r.eras.len() > 1 {
        slice_table(&mut out, "by era", &r.eras);
    }
    let _ = writeln!(
        out,
        "\ntime: collect {:.1}s, embed {:.1}s ({} file versions embedded, {} reused), score {:.1}s",
        r.timing.collect_s,
        r.timing.embed_s,
        r.timing.docs_embedded,
        r.timing.docs_reused,
        r.timing.score_s
    );
    for n in &r.notes {
        let _ = writeln!(out, "note: {n}");
    }
    let _ = writeln!(
        out,
        "hit@k: a changed file among the top k suggestions; see benchmarks/history-replay.md"
    );
    out
}

/// Prints progress to stderr (so `--json` output stays clean).
pub fn stderr_log(msg: &str) {
    let _ = writeln!(std::io::stderr(), "wn bench: {msg}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_candidate_blob_skips_commit_instead_of_scoring_empty_text() {
        let mut docs = Docs::default();
        let present = docs.id("src/present.rs", "present");
        let missing = docs.id("src/missing.rs", "missing");
        let task = |sha: &str, cand: Vec<usize>| Task {
            sha: sha.into(),
            date: String::new(),
            query: QueryInput::file("present"),
            cand,
            gold: vec![0],
        };
        let mut tasks = vec![
            task("skipped", vec![present, missing]),
            task("kept", vec![present]),
        ];
        let texts = HashMap::from([("present".to_string(), "real source".to_string())]);

        let kept = keep_readable_tasks(&mut tasks, &mut docs, &texts);

        assert_eq!(kept, vec![1]);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].sha, "kept");
        assert_eq!(docs.keys.len(), 1);
        assert_eq!(docs.text[0], file_doc("src/present.rs", "real source"));
    }

    #[test]
    fn legacy_vectors_from_unreadable_blobs_are_not_reused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("bench-vectors.json"),
            serde_json::json!({"fingerprint":"model", "dim":1, "keys":["a.rs\tblob"]}).to_string(),
        )
        .unwrap();
        std::fs::write(dir.path().join("bench-vectors.bin"), 0f32.to_le_bytes()).unwrap();
        assert!(cache_load(dir.path(), "model").is_empty());
    }

    #[test]
    fn tokens_match_the_reference_regex() {
        assert_eq!(
            tokens("HTTPServer handleLogin snake_case x A1 v2 parseJSON URLs a b"),
            vec![
                "http", "server", "handle", "login", "snake", "case", "a", "1", "2", "parse",
                "json", "ur", "ls"
            ]
        );
        assert_eq!(
            tokens("file: src/auth.py\nLogin sessions"),
            vec!["file", "src", "auth", "py", "login", "sessions"]
        );
        assert!(tokens("é ü ___ ").is_empty());
    }

    #[test]
    fn gold_rank_is_the_best_gold_position_with_stable_ties() {
        assert_eq!(gold_rank(&[0.1, 0.9, 0.5], &[2]), Some(1));
        assert_eq!(gold_rank(&[0.1, 0.9, 0.5], &[0, 2]), Some(1));
        assert_eq!(gold_rank(&[0.5, 0.5, 0.5], &[2]), Some(2));
        assert_eq!(gold_rank(&[0.5, 0.5], &[]), None);
    }

    #[test]
    fn scores_count_misses() {
        let s = score("m", &[Some(0), Some(2), Some(9), None]);
        assert_eq!((s.n, s.hit1, s.hit3, s.hit10), (4, 0.25, 0.5, 0.75));
        assert!((s.mrr - round3((1.0 + 1.0 / 3.0 + 0.1) / 4.0)).abs() < 1e-9);
    }

    #[test]
    fn eras_split_on_directory_and_language_changes() {
        let set = |xs: &[&str]| xs.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
        let dirs: Vec<HashSet<String>> = (0..10)
            .map(|i| {
                if i < 5 {
                    set(&["a/x", "a/y"])
                } else {
                    set(&["b/z"])
                }
            })
            .collect();
        let langs = vec!["py".to_string(); 10];
        assert_eq!(eras(&dirs, &langs, 0.4, 2), vec![0, 5]);
        assert_eq!(
            eras(&dirs, &langs, 0.4, 6),
            vec![0],
            "short eras merge into the previous one"
        );
        let mut langs2 = langs.clone();
        for l in &mut langs2[7..] {
            *l = "go".into();
        }
        assert_eq!(eras(&dirs, &langs2, 0.4, 2), vec![0, 5, 7]);
        assert!(eras(&[], &[], 0.4, 2).is_empty());
    }

    #[test]
    fn contextbench_paths_are_made_repository_relative() {
        assert_eq!(
            repo_relative("/workspace/owner__repo__0.1/src/a.rs"),
            "src/a.rs"
        );
        assert_eq!(repo_relative("src/a.rs"), "src/a.rs");
        assert_eq!(repo_relative("./src/a.rs"), "src/a.rs");
    }
}
