//! Answering a query end to end: fit or load the personal adapter, rank, and decide whether to
//! answer, abstain or fail open.

use std::fs;
use std::io;
use std::path::Path;

use half::f16;
use serde::{Deserialize, Serialize};

use crate::adapter::{
    apply_adapter, fit_adapter, revision, sample_negatives, AdapterParams, ADAPTER_MIN_TRAIN,
};
use crate::encoder::{EncodeError, Encoder, QueryInput};
use crate::index::{EntryKind, Index};
use crate::query_lifecycle::{QueryEvent, QueryLifecycle, QueryState};
use crate::rank::{
    abstain_with, budget, AdapterUse, AnswerState, Hint, Hints, Outcome, QueryKind, MAX_HINTS,
    MIN_SHOWN_SIMILARITY,
};
use crate::text::{history_body, Granularity};

/// A past commit, as used to fit the adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryExample {
    /// Commit SHA.
    pub sha: String,
    /// Commit date.
    pub date: String,
    /// Message subject.
    pub subject: String,
    /// Message body.
    pub body: String,
    /// Changed paths.
    pub paths: Vec<String>,
}

/// Metadata stored with a fitted adapter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdapterMeta {
    /// Content revision of the weights.
    pub revision: String,
    /// Fingerprint of the model the adapter was fitted against.
    pub base: String,
    /// Vector dimension.
    pub dim: usize,
    /// Training examples used.
    pub n_train: usize,
    /// Examples from commit history.
    pub n_history: usize,
    /// Examples from logged usage.
    pub n_usage: usize,
    /// Newest commit used (fits only ever look at commits up to here).
    pub history_cutoff: Option<String>,
    /// Date of the newest commit used.
    pub history_cutoff_date: Option<String>,
    /// Date of the oldest commit used.
    pub oldest: Option<String>,
    /// Seconds the fit took.
    pub fit_seconds: f64,
}

/// A fitted adapter: metadata plus row-major `dim × dim` weights.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredAdapter {
    /// Metadata.
    pub meta: AdapterMeta,
    /// Weights.
    pub w: Vec<f32>,
}

/// Why no adapter was fitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FitSkipped {
    /// Too few commits touch files that are indexed today.
    TooFewExamples {
        /// Usable examples found.
        found: usize,
    },
    /// Fewer than two indexed files.
    TooFewFiles,
    /// The encoder failed.
    Encode(String),
}

impl std::fmt::Display for FitSkipped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FitSkipped::TooFewExamples { found } => {
                write!(f, "only {found} usable examples (need {ADAPTER_MIN_TRAIN})")
            }
            FitSkipped::TooFewFiles => write!(f, "fewer than two indexed files"),
            FitSkipped::Encode(e) => write!(f, "embedding failed: {e}"),
        }
    }
}

/// Fits the adapter on past commits whose changed files are indexed today, plus logged usage
/// (`(query, edited paths)`). The first indexed path a commit changed is its positive.
pub fn fit_from_history(
    index: &Index,
    commits: &[HistoryExample],
    usage: &[(QueryInput, Vec<String>)],
    encoder: &dyn Encoder,
    params: &AdapterParams,
    limit: usize,
    seed: u64,
) -> Result<StoredAdapter, FitSkipped> {
    let started = std::time::Instant::now();
    let (rows, cand) = index.matrix(EntryKind::File);
    let d = index.dim();
    let position: std::collections::HashMap<&str, usize> = rows
        .iter()
        .enumerate()
        .map(|(k, e)| (e.path.as_str(), k))
        .collect();
    let mut queries = Vec::new();
    let mut pos = Vec::new();
    let mut used = Vec::new();
    for c in commits {
        if let Some(hit) = c.paths.iter().find_map(|p| position.get(p.as_str())) {
            queries.push(QueryInput::file(history_body(&c.subject, &c.body)));
            pos.push(*hit);
            used.push(c);
        }
        if used.len() >= limit {
            break;
        }
    }
    let n_history = queries.len();
    for (q, edited) in usage {
        if let Some(hit) = edited.iter().find_map(|p| position.get(p.as_str())) {
            queries.push(q.clone());
            pos.push(*hit);
        }
    }
    if rows.len() < 2 {
        return Err(FitSkipped::TooFewFiles);
    }
    if queries.len() < ADAPTER_MIN_TRAIN {
        return Err(FitSkipped::TooFewExamples {
            found: queries.len(),
        });
    }
    let negs = sample_negatives(&pos, rows.len(), params.negatives, seed);
    let qv: Vec<f32> = encoder
        .queries(&queries)
        .map_err(|e| FitSkipped::Encode(e.0))?
        .into_iter()
        .flatten()
        .collect();
    let w = fit_adapter(&qv, &cand, d, &pos, &negs, params);
    Ok(StoredAdapter {
        meta: AdapterMeta {
            revision: revision(&w),
            base: encoder.fingerprint(),
            dim: d,
            n_train: queries.len(),
            n_history,
            n_usage: queries.len() - n_history,
            history_cutoff: used.first().map(|c| c.sha.clone()),
            history_cutoff_date: used.first().map(|c| c.date.clone()),
            oldest: used.last().map(|c| c.date.clone()),
            fit_seconds: (started.elapsed().as_secs_f64() * 100.0).round() / 100.0,
        },
        w,
    })
}

/// Writes an adapter to `dir` (weights stored as float16), replacing any previous one atomically.
pub fn save_adapter(dir: &Path, adapter: &StoredAdapter) -> io::Result<()> {
    let parent = dir.parent().unwrap_or(dir);
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!("adapter.tmp.{}", std::process::id()));
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&tmp)?;
    let mut bytes = Vec::with_capacity(adapter.w.len() * 2);
    for x in &adapter.w {
        bytes.extend_from_slice(&f16::from_f32(*x).to_le_bytes());
    }
    fs::write(tmp.join("w.f16"), bytes)?;
    fs::write(
        tmp.join("meta.json"),
        serde_json::to_vec_pretty(&adapter.meta).map_err(io::Error::other)?,
    )?;
    let old = parent.join(format!("adapter.old.{}", std::process::id()));
    if dir.exists() {
        fs::rename(dir, &old)?;
    }
    fs::rename(&tmp, dir)?;
    let _ = fs::remove_dir_all(&old);
    Ok(())
}

/// Reads an adapter from `dir`, if a valid one is there.
pub fn load_adapter(dir: &Path) -> Option<StoredAdapter> {
    let meta: AdapterMeta = serde_json::from_slice(&fs::read(dir.join("meta.json")).ok()?).ok()?;
    let raw = fs::read(dir.join("w.f16")).ok()?;
    if raw.len() != meta.dim * meta.dim * 2 {
        return None;
    }
    let w = raw
        .chunks_exact(2)
        .map(|b| f16::from_le_bytes([b[0], b[1]]).to_f32())
        .collect();
    Some(StoredAdapter { meta, w })
}

/// Options for [`suggest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SuggestOptions {
    /// Maximum file hints.
    pub k: usize,
    /// Also rank definitions.
    pub with_functions: bool,
    /// Use the adapter when one matches the model.
    pub adapt: bool,
    /// Use the strict abstain thresholds.
    pub strict_abstain: bool,
    /// Never abstain (always return the ranking).
    pub no_abstain: bool,
    /// The repository has files, but none of an indexable kind.
    pub unsupported_only: bool,
    /// A task-start hint: skip (abstain) when the index has fewer source files than this.
    pub start_min_files: Option<usize>,
}

impl Default for SuggestOptions {
    fn default() -> Self {
        Self {
            k: MAX_HINTS,
            with_functions: false,
            adapt: true,
            strict_abstain: false,
            no_abstain: false,
            unsupported_only: false,
            start_min_files: None,
        }
    }
}

fn step(q: &mut QueryLifecycle, e: QueryEvent) {
    q.handle(e)
        .unwrap_or_else(|err| panic!("query lifecycle: {err}"));
}

/// Error for a query with no text: nothing to rank.
pub const EMPTY_QUERY: &str =
    "empty query: say what you are looking for, e.g. wn ask \"where is the request logger\"";

/// The error outcome for an empty query (blank request and blank context), else `None`.
/// [`suggest`] checks it first; the CLI checks it before touching the index.
pub fn empty_query(query: &str, context: &str) -> Option<Outcome> {
    (query.trim().is_empty() && context.trim().is_empty()).then(|| Outcome {
        state: AnswerState::Error,
        error: Some(EMPTY_QUERY.into()),
        ..Outcome::default()
    })
}

/// Answers one query against an index. Operational problems never raise: they fail open with a
/// machine-readable state, so the caller falls back to its normal search.
pub fn suggest(
    index: &Index,
    adapter: Option<&StoredAdapter>,
    encoder: &dyn Encoder,
    query: &str,
    context: &str,
    opts: SuggestOptions,
) -> Outcome {
    let mut life = QueryLifecycle::default();
    let fail = |state: AnswerState, error: Option<String>| Outcome {
        state,
        error,
        ..Outcome::default()
    };
    if let Some(empty) = empty_query(query, context) {
        step(&mut life, QueryEvent::Unavailable);
        return empty;
    }
    if !index.state().can_serve() {
        step(&mut life, QueryEvent::Unavailable);
        return fail(
            AnswerState::Error,
            Some(format!("index is {:?}", index.state())),
        );
    }
    if index.count(EntryKind::File) == 0 && index.count(EntryKind::Config) == 0 {
        step(&mut life, QueryEvent::Unavailable);
        let state = if opts.unsupported_only {
            AnswerState::UnsupportedScope
        } else {
            AnswerState::EmptyIndex
        };
        return fail(state, None);
    }
    let input = QueryInput {
        query: query.to_string(),
        context: context.to_string(),
        granularity: Granularity::File,
    };
    let mut q = match encoder.queries(std::slice::from_ref(&input)) {
        Ok(mut v) if !v.is_empty() => v.swap_remove(0),
        Ok(_) => {
            step(&mut life, QueryEvent::Unavailable);
            return fail(AnswerState::Error, Some("encoder returned nothing".into()));
        }
        Err(EncodeError(e)) => {
            step(&mut life, QueryEvent::Unavailable);
            return fail(AnswerState::Error, Some(e));
        }
    };
    let mut adapter_use = AdapterUse {
        applied: false,
        revision: None,
        reason: Some("no adapter".into()),
    };
    if !opts.adapt {
        adapter_use.reason = Some("disabled".into());
    } else if let Some(a) = adapter {
        if a.meta.base == encoder.fingerprint() && a.meta.dim == q.len() {
            q = apply_adapter(&q, &a.w, a.meta.dim);
            adapter_use = AdapterUse {
                applied: true,
                revision: Some(a.meta.revision.clone()),
                reason: None,
            };
        } else {
            adapter_use.reason =
                Some("identity fallback: adapter was fitted for another model".into());
        }
    }
    let files = index.rank(&q, EntryKind::File, opts.k.max(2));
    let configs = index.rank(&q, EntryKind::Config, 1);
    let functions = if opts.with_functions {
        let fin = QueryInput {
            granularity: Granularity::Function,
            ..input
        };
        match encoder.queries(std::slice::from_ref(&fin)) {
            Ok(v) if !v.is_empty() => index.rank(&v[0], EntryKind::Function, opts.k),
            _ => Vec::new(),
        }
    } else {
        Vec::new()
    };
    step(&mut life, QueryEvent::Ranked);
    let n_files = index.count(EntryKind::File);
    let small = opts.start_min_files.filter(|&min| n_files < min);
    let reason = if let Some(min) = small {
        Some(format!(
            "start: {n_files} files < {min}; start hints help in large repositories"
        ))
    } else if opts.no_abstain {
        None
    } else {
        let kind = QueryKind::classify(query, context);
        let no_match = || {
            files
                .first()
                .is_some_and(|h| h.similarity < MIN_SHOWN_SIMILARITY)
                .then(|| "no file matches the query".to_string())
        };
        match encoder.calibration() {
            // Uncalibrated (the lexical fallback): abstain only when nothing matches.
            None => no_match(),
            Some(c) => match c.thresholds(kind, adapter_use.applied, opts.strict_abstain) {
                // `null` or a negative `min_top`: this kind never abstains.
                None => None,
                Some(th) if th.min_top < 0.0 => None,
                Some(th) => abstain_with(&files, th)
                    .map(|why| format!("{}: {why}", kind.as_str()))
                    .or_else(no_match),
            },
        }
    };
    if let Some(reason) = reason {
        step(&mut life, QueryEvent::NotConfident);
        return Outcome {
            state: AnswerState::Abstain,
            abstain: Some(reason),
            adapter: adapter_use,
            ..Outcome::default()
        };
    }
    step(&mut life, QueryEvent::Confident);
    debug_assert_eq!(life.state(), QueryState::Answer);
    let mut files = files;
    files.truncate(opts.k);
    let shown = |h: &Hint| h.similarity >= MIN_SHOWN_SIMILARITY;
    // Only reachable with abstaining off: keep the top hint rather than answer with nothing.
    let top = files.first().cloned();
    files.retain(shown);
    if files.is_empty() {
        files.extend(top);
    }
    let (mut functions, mut configs) = (functions, configs);
    functions.retain(shown);
    configs.retain(shown);
    Outcome {
        state: AnswerState::Ok,
        hints: budget(Hints {
            files,
            functions,
            configs,
        }),
        abstain: None,
        error: None,
        adapter: adapter_use,
    }
}
