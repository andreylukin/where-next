//! Ranking, abstaining and the hint budget.
//!
//! where-next is a search accelerator, not an oracle: it returns at most three short hints, and
//! when the top result is not clearly better than chance it abstains so the agent falls back to
//! its normal search. Abstain thresholds come from the model's [`Calibration`], per
//! [`QueryKind`]; a model without one abstains only when nothing matches at all.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Maximum paths returned per answer (files, functions and configs together).
pub const MAX_HINTS: usize = 3;
/// Default minimum source files for automatic task-start hints: in the agent trials the start
/// hint paid off only in large repositories.
pub const START_HINT_MIN_FILES: usize = 3000;
/// Approximate token budget for the hint text.
pub const TOKEN_BUDGET: usize = 250;

/// Abstain thresholds on the top cosine similarity and its margin over the second result.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
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

/// Adapter-mode thresholds used when a calibration leaves them unset (0.0/0.0). The shipped
/// gemma calibrations fitted "never abstain" with the adapter on their fitting repository, which
/// let gibberish through with 3 hints. Measured with gemma-xl1 + adapter on gin, axum and
/// fastapi: 55 real questions scored 0.225-0.63 at the top, 60 vague or gibberish queries
/// 0.05-0.33 (most below 0.2).
pub const ADAPTER_FLOOR: Thresholds = Thresholds {
    min_top: 0.2,
    min_margin: 0.0,
};

/// Hints below this similarity print as 0.00: they are never shown, and a top hint below it
/// means nothing matched.
pub const MIN_SHOWN_SIMILARITY: f64 = 0.005;

/// v2b thresholds for error queries, fitted on a held-out eval of ~1.2k error-carrying queries
/// (SWE-bench Verified, SWE-PolyBench, Multi-SWE-bench, LCA, ContextBench, SWE-Gym mid-trajectory)
/// on dev repositories and checked on test repositories (answered hit@3 target 0.85 / 0.92 strict).
pub const V2B_ERROR: KindThresholds = KindThresholds {
    adapter: Thresholds {
        min_top: 0.5193,
        min_margin: 0.002,
    },
    plain: Thresholds {
        min_top: 0.5138,
        min_margin: 0.0409,
    },
    strict_adapter: Thresholds {
        min_top: 0.5193,
        min_margin: 0.0227,
    },
    strict_plain: Thresholds {
        min_top: 0.6539,
        min_margin: 0.065,
    },
};

/// What kind of query this is; each kind can have its own abstain thresholds, because
/// similarities run lower for long issue text than for short requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryKind {
    /// A short request with no context (like a commit message).
    Request,
    /// An issue or task description: long, or multi-line, with no context.
    Issue,
    /// Anything carrying an error, stack trace or failing test.
    Error,
    /// A follow-up in a conversation: recent turns as context, no error.
    Conversational,
}

/// Patterns that only appear when a tool or runtime printed an error: stack frames, error lines,
/// failing tests, crashes. One match is enough.
const STRONG_ERROR: &[&str] = &[
    r"Traceback \(most recent call last\)",
    r#"(?m)^\s*File "[^"]+", line \d+"#,
    r"(?m)^\s+at [\w$.<>\[\]/]+ ?\(.*:\d+(:\d+)?\)",
    r"(?m)^\s+at .+:\d+:\d+\s*$",
    r"(?m)^\s*(\w+\.)*[A-Z]\w*(Error|Exception)(: |:$|\()",
    r"(?m)^\s*(error|Error|ERROR)(\[E\d+\])?: ",
    r"(?m)^[^\s:]+:\d+(:\d+)?: (fatal )?error: ",
    r"(?m)^\s*(thread '.+' )?panicked at ",
    r"(?m)^panic: |\bpanic(ked|s)?\b",
    r"(?m)^(FAILED|FAIL)[: ]|\bFAILED\b",
    r"(?m)^E\s{3,}\S",
    r"\bAssertionError\b",
    r"Segmentation fault|core dumped|SIGSEGV",
    r"undefined reference to",
    r"(?m)^npm ERR!",
    r"(?m)^fatal: ",
    r"exit (code|status) [1-9]\d*\b",
    r"\bUncaught \w+",
    r"(?i)\bstack ?trace\b",
    r"\bTraceback\b",
    r#"(?m)\b[A-Z][A-Za-z0-9]*(Error|Exception)(:|\(|\s+['"]|\s*$)"#,
    r"\bError: \S",
    r"\bException(:|\s+in\s)",
];
/// Words that suggest an error in prose; they count only in pairs, in code-looking text.
const WEAK_ERROR: &str =
    r"(?i)\b(error|errors|exception|crash(es|ed)?|fails?|failing|failed|broken|raises?)\b";

fn error_patterns() -> &'static (Vec<regex::Regex>, regex::Regex) {
    static PATTERNS: std::sync::OnceLock<(Vec<regex::Regex>, regex::Regex)> =
        std::sync::OnceLock::new();
    PATTERNS.get_or_init(|| {
        (
            STRONG_ERROR
                .iter()
                .map(|p| regex::Regex::new(p).expect("valid error pattern"))
                .collect(),
            regex::Regex::new(WEAK_ERROR).expect("valid weak error pattern"),
        )
    })
}

/// Whether text carries a real error (a stack trace, an error line, a failing test, a crash).
/// A class name like `ErrorBoundary` or a single "error" in prose is not enough.
pub fn has_error(text: &str) -> bool {
    if text.is_empty() {
        return false;
    }
    let (strong, weak) = error_patterns();
    if strong.iter().any(|p| p.is_match(text)) {
        return true;
    }
    weak.find_iter(text).count() >= 2 && (text.trim().contains('\n') || text.contains('`'))
}

/// Longest request (in characters) still treated as a conversational follow-up.
pub const FOLLOW_UP_MAX_CHARS: usize = 300;
/// Most lines in a request still treated as a conversational follow-up.
pub const FOLLOW_UP_MAX_LINES: usize = 3;

impl QueryKind {
    /// Classifies a query from its request and context text (the Python reference is
    /// `harness_router.querykind.classify`, which the calibration eval used).
    pub fn classify(query: &str, context: &str) -> QueryKind {
        let (query, context) = (query.trim(), context.trim());
        if has_error(query) || has_error(context) {
            return QueryKind::Error;
        }
        let lines = query.lines().count().max(1);
        let chars = query.chars().count();
        if !context.is_empty() && chars <= FOLLOW_UP_MAX_CHARS && lines <= FOLLOW_UP_MAX_LINES {
            return QueryKind::Conversational;
        }
        if lines > 1 || chars >= 200 {
            QueryKind::Issue
        } else {
            QueryKind::Request
        }
    }

    /// Name used in calibration files and reasons.
    pub fn as_str(self) -> &'static str {
        match self {
            QueryKind::Request => "request",
            QueryKind::Issue => "issue",
            QueryKind::Error => "error",
            QueryKind::Conversational => "conversational",
        }
    }
}

/// Thresholds for one query kind, with and without the adapter, default and strict.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct KindThresholds {
    /// Adapter applied.
    pub adapter: Thresholds,
    /// No adapter.
    pub plain: Thresholds,
    /// Adapter applied, strict (fewer, more precise answers).
    pub strict_adapter: Thresholds,
    /// No adapter, strict.
    pub strict_plain: Thresholds,
}

/// A model's abstain calibration (`calibration.json` next to the model). `kinds` maps a
/// [`QueryKind`] name (or `default`) to thresholds; `null` (or a negative `min_top`) means that
/// kind never abstains. Kinds not listed use `default`; without `default` they never abstain.
/// Adapter thresholds of 0.0/0.0 count as unset and use [`ADAPTER_FLOOR`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Calibration {
    /// Model the thresholds were fitted for.
    pub model: String,
    /// Data the thresholds were fitted on.
    #[serde(default)]
    pub calibrated_on: String,
    /// Thresholds per kind.
    pub kinds: BTreeMap<String, Option<KindThresholds>>,
}

impl Calibration {
    /// The reference model's calibration: the research thresholds for requests (fitted on a
    /// real repository history), no abstaining on issue-style starts (the agent trial showed
    /// those thresholds withhold mostly-correct hints on issue text).
    pub fn v2b() -> Calibration {
        let default = KindThresholds {
            adapter: ABSTAIN_ADAPTER,
            plain: ABSTAIN_PLAIN,
            strict_adapter: ABSTAIN_STRICT_ADAPTER,
            strict_plain: ABSTAIN_STRICT_PLAIN,
        };
        Calibration {
            model: "v2b".into(),
            calibrated_on:
                "repository history (commit-message queries); error: held-out benchmark eval".into(),
            kinds: BTreeMap::from([
                ("default".to_string(), Some(default)),
                ("issue".to_string(), None),
                ("error".to_string(), Some(V2B_ERROR)),
            ]),
        }
    }

    /// Thresholds for a query, or `None` when this kind never abstains.
    pub fn thresholds(&self, kind: QueryKind, adapted: bool, strict: bool) -> Option<Thresholds> {
        let entry = match self.kinds.get(kind.as_str()) {
            Some(listed) => *listed,
            None => self.kinds.get("default").copied().flatten(),
        }?;
        let th = match (strict, adapted) {
            (false, true) => entry.adapter,
            (false, false) => entry.plain,
            (true, true) => entry.strict_adapter,
            (true, false) => entry.strict_plain,
        };
        let unset = th.min_top == 0.0 && th.min_margin == 0.0;
        Some(if adapted && unset { ADAPTER_FLOOR } else { th })
    }
}

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

/// Why an answer should abstain under the reference model's request thresholds, or `None` to
/// answer; `fallback` models never abstain. (Kept to pin behaviour to the reference.)
pub fn abstain_reason(
    files: &[Hint],
    adapted: bool,
    strict: bool,
    fallback: bool,
) -> Option<String> {
    if fallback {
        return None;
    }
    let th = Calibration::v2b().thresholds(QueryKind::Request, adapted, strict)?;
    abstain_with(files, th)
}

/// Why `files` should abstain under `th`, or `None` to answer.
pub fn abstain_with(files: &[Hint], th: Thresholds) -> Option<String> {
    if files.is_empty() {
        return None;
    }
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
