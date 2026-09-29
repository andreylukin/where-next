//! Ranking, abstaining and the hint budget.
//!
//! where-next is a search accelerator, not an oracle: it returns at most three short hints, and
//! when the top result is not clearly better than chance for the pinned model it abstains so the
//! agent falls back to its normal search.

use serde::{Deserialize, Serialize};

/// Maximum paths returned per answer (files, functions and configs together).
pub const MAX_HINTS: usize = 3;
/// Approximate token budget for the hint text.
pub const TOKEN_BUDGET: usize = 250;

/// Abstain thresholds on the top cosine similarity and its margin over the second result.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Thresholds {
    /// Minimum similarity of the top result.
    pub min_top: f64,
    /// Minimum margin of the top result over the second.
    pub min_margin: f64,
}

/// Default thresholds when the personal adapter is applied (calibrated on held-out repository
/// history for the pinned model; target ≥ 0.75 answered hit@3).
pub const ABSTAIN_ADAPTER: Thresholds = Thresholds {
    min_top: 0.274,
    min_margin: 0.0015,
};
/// Default thresholds without an adapter.
pub const ABSTAIN_PLAIN: Thresholds = Thresholds {
    min_top: 0.5757,
    min_margin: 0.0024,
};
/// Strict thresholds with the adapter (~0.9 answered hit@3 at ~35% coverage).
pub const ABSTAIN_STRICT_ADAPTER: Thresholds = Thresholds {
    min_top: 0.4496,
    min_margin: 0.0606,
};
/// Strict thresholds without an adapter.
pub const ABSTAIN_STRICT_PLAIN: Thresholds = Thresholds {
    min_top: 0.5629,
    min_margin: 0.0333,
};

/// One ranked location.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hint {
    /// Repository-relative path.
    pub path: String,
    /// Cosine similarity to the query (not a probability).
    pub similarity: f64,
    /// Definition name, for function hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// 1-based line, for function hints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

/// Hints by kind.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Hints {
    /// Source files.
    #[serde(default)]
    pub files: Vec<Hint>,
    /// Definitions inside source files.
    #[serde(default)]
    pub functions: Vec<Hint>,
    /// Config files (Dockerfile, YAML, …).
    #[serde(default)]
    pub configs: Vec<Hint>,
}

/// Machine-readable state of an answer. Everything except `Ok` means "use normal search".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum AnswerState {
    /// Confident hints.
    #[default]
    Ok,
    /// Not confident enough; no hints.
    Abstain,
    /// Nothing indexed yet.
    EmptyIndex,
    /// The repository has files, but none of a supported kind.
    UnsupportedScope,
    /// Serving from an out-of-date index.
    StaleIndex,
    /// An operational error (model missing, repository unreadable, …).
    Error,
}

/// Whether the personal adapter was used for this answer.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct AdapterUse {
    /// The query was mapped through the adapter.
    #[serde(default)]
    pub applied: bool,
    /// Adapter revision, when applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// Why it was not applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The result of one query, as rendered for agents.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Outcome {
    /// Answer state.
    pub state: AnswerState,
    /// Ranked hints (empty unless `state` is `Ok` or `StaleIndex`).
    #[serde(flatten)]
    pub hints: Hints,
    /// Why the answer abstained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstain: Option<String>,
    /// Error description for `Error`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Adapter use.
    #[serde(default)]
    pub adapter: AdapterUse,
}

/// Top `k` rows of the row-major matrix `mat` (`rows × d`) by dot product with `q`, highest
/// first; ties keep index order. Returns `(row, score)`.
pub fn top_k(mat: &[f32], d: usize, q: &[f32], k: usize) -> Vec<(usize, f32)> {
    assert_eq!(q.len(), d);
    let scored: Vec<(usize, f32)> = mat
        .chunks_exact(d)
        .enumerate()
        .map(|(i, row)| (i, row.iter().zip(q).map(|(a, b)| a * b).sum()))
        .collect();
    select_top(scored, k)
}

/// The `k` highest-scoring `(id, score)` pairs, best first; ties go to the smaller id.
pub fn select_top(mut scored: Vec<(usize, f32)>, k: usize) -> Vec<(usize, f32)> {
    let k = k.min(scored.len());
    if k == 0 {
        return Vec::new();
    }
    let cmp = |a: &(usize, f32), b: &(usize, f32)| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    };
    if k < scored.len() {
        scored.select_nth_unstable_by(k - 1, cmp);
        scored.truncate(k);
    }
    scored.sort_by(cmp);
    scored
}

/// Rounds a similarity to 4 decimals for output, as the reference does.
pub fn round4(x: f32) -> f64 {
    (x as f64 * 10_000.0).round() / 10_000.0
}

/// Why an answer should abstain, or `None` to answer. Thresholds are calibrated for the pinned
/// model only: other (`fallback`) models never abstain.
pub fn abstain_reason(
    files: &[Hint],
    adapted: bool,
    strict: bool,
    fallback: bool,
) -> Option<String> {
    if files.is_empty() || fallback {
        return None;
    }
    let th = match (strict, adapted) {
        (false, true) => ABSTAIN_ADAPTER,
        (false, false) => ABSTAIN_PLAIN,
        (true, true) => ABSTAIN_STRICT_ADAPTER,
        (true, false) => ABSTAIN_STRICT_PLAIN,
    };
    let top = files[0].similarity;
    let margin = top - files.get(1).map(|h| h.similarity).unwrap_or(0.0);
    if top < th.min_top {
        return Some(format!("top similarity {top:.2} < {:.2}", th.min_top));
    }
    if margin < th.min_margin {
        return Some(format!("margin {margin:.3} < {:.3}", th.min_margin));
    }
    None
}

fn cost(items: &[&Hint]) -> usize {
    items
        .iter()
        .map(|h| h.path.chars().count() + h.name.as_deref().map_or(0, |n| n.chars().count()) + 12)
        .sum::<usize>()
        / 4
}

/// Keeps at most [`MAX_HINTS`] paths overall and about [`TOKEN_BUDGET`] tokens of hint text:
/// files first, then functions, then one config; over budget, functions go first, then configs,
/// then the lowest-ranked files.
pub fn budget(mut hints: Hints) -> Hints {
    hints.files.truncate(MAX_HINTS);
    let room = MAX_HINTS - hints.files.len();
    hints.functions.truncate(room);
    let cfg_room = if hints.files.len() + hints.functions.len() < MAX_HINTS {
        1
    } else {
        0
    };
    hints.configs.truncate(cfg_room);
    while !hints.files.is_empty() {
        let all: Vec<&Hint> = hints
            .files
            .iter()
            .chain(&hints.functions)
            .chain(&hints.configs)
            .collect();
        if cost(&all) <= TOKEN_BUDGET {
            break;
        }
        if !hints.functions.is_empty() {
            hints.functions.pop();
        } else if !hints.configs.is_empty() {
            hints.configs.pop();
        } else {
            hints.files.pop();
        }
    }
    hints
}

fn state_name(state: AnswerState) -> &'static str {
    match state {
        AnswerState::Ok => "ok",
        AnswerState::Abstain => "abstain",
        AnswerState::EmptyIndex => "empty_index",
        AnswerState::UnsupportedScope => "unsupported_scope",
        AnswerState::StaleIndex => "stale_index",
        AnswerState::Error => "error",
    }
}

/// Compact text for agents: at most three paths, a few hundred characters.
pub fn render(out: &Outcome) -> String {
    match out.state {
        AnswerState::Abstain => format!(
            "where-next: no confident hint ({}); use normal search.",
            out.abstain.as_deref().unwrap_or("")
        ),
        AnswerState::Ok => {
            let mut lines = vec![format!(
                "where-next hints (cosine similarity; adapter {}):",
                if out.adapter.applied { "on" } else { "off" }
            )];
            for h in &out.hints.files {
                lines.push(format!("{:.2}  {}", h.similarity, h.path));
            }
            for h in &out.hints.functions {
                lines.push(format!(
                    "{:.2}  {}:{}  {}",
                    h.similarity,
                    h.path,
                    h.line.unwrap_or(0),
                    h.name.as_deref().unwrap_or("")
                ));
            }
            for h in &out.hints.configs {
                lines.push(format!("{:.2}  {}  (config)", h.similarity, h.path));
            }
            lines.join("\n")
        }
        other => {
            let detail = out.error.as_deref().unwrap_or("");
            format!(
                "where-next: {} ({}); use normal search.",
                state_name(other),
                detail
            )
            .replace(" ()", "")
        }
    }
}

/// Reciprocal-rank fusion of several full orderings of the same items (highest fused score
/// first; ties keep first-seen order).
pub fn rrf(orders: &[&[usize]], k: usize) -> Vec<usize> {
    let mut score: Vec<(usize, f64)> = Vec::new();
    for order in orders {
        for (r, &i) in order.iter().enumerate() {
            let add = 1.0 / (k + r + 1) as f64;
            match score.iter_mut().find(|(j, _)| *j == i) {
                Some((_, s)) => *s += add,
                None => score.push((i, add)),
            }
        }
    }
    let mut idx: Vec<usize> = (0..score.len()).collect();
    idx.sort_by(|&a, &b| {
        score[b]
            .1
            .partial_cmp(&score[a].1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.cmp(&b))
    });
    idx.into_iter().map(|j| score[j].0).collect()
}

/// Position of the first wanted item in a ranking.
pub fn rank_of(order: &[usize], wanted: &[usize]) -> Option<usize> {
    order.iter().position(|i| wanted.contains(i))
}
