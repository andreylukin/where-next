//! `wn stats`: how wn is doing for you, from the local usage log, your agents' transcripts
//! (read locally; see [`crate::agents`]) and git.
//!
//! The headline is whether your agent acted on a hint: after a `wn ask`, did its next tool calls
//! open or edit a hinted file? Then: whether *you* changed a hinted file within a day (git), the
//! latest `wn bench` replay against grep-style search, speed, and query volume.
//!
//! `--share` renders a [`ShareCard`], a separate type that holds only numbers and allowlisted
//! enums (no repository names, paths or queries), as text or as a self-contained SVG.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use wn_daemon::usage::{RepoUsage, RETENTION_DAYS};

use crate::agents::{Action, Agent, AgentCounts, AgentScore, Similarity};
use crate::hooks::HookRun;
use crate::report::{hint_edits, percentile, round_ms, EditedFn, KnownModel, Language};

const DAY: u64 = 86_400;
/// Below this many answers a share is shown as "n of m", never as a percentage.
pub const MIN_FOR_PERCENT: usize = 5;
/// Width of a bar in cells.
const BAR: usize = 20;
/// Width of the label column.
const LABEL: usize = 22;

/// "n of m".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
pub struct Ratio {
    pub n: usize,
    pub of: usize,
}

impl Ratio {
    pub fn new(n: usize, of: usize) -> Ratio {
        Ratio { n: n.min(of), of }
    }

    /// The share, when there are enough answers to quote one.
    pub fn rate(self) -> Option<f64> {
        (self.of >= MIN_FOR_PERCENT).then(|| self.n as f64 / self.of as f64)
    }

    fn pct(self) -> Option<u32> {
        self.rate().map(|r| (r * 100.0).round() as u32)
    }
}

/// What agents did with wn's answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct AgentSummary {
    /// False when transcripts were not read (`--no-agents`, `WN_STATS_NO_AGENTS`).
    pub scanned: bool,
    /// wn calls found in transcripts and matched to a logged answer.
    pub calls: usize,
    /// Of those, answers with hints; the four shares below partition them.
    pub answered: usize,
    /// The agent then touched a hinted file.
    pub exact: Ratio,
    /// It touched a file next to a hint (same directory, or its test ↔ source pair).
    pub near: Ratio,
    /// It touched other files.
    pub elsewhere: Ratio,
    /// It touched no files before moving on.
    pub no_files: Ratio,
    /// Exact answers by the strongest thing done to a hinted file.
    pub exact_read: usize,
    pub exact_ran: usize,
    pub exact_edited: usize,
    /// Of the exact answers, those where the first hinted file touched was ranked #1.
    pub first_pick: Ratio,
    /// Sessions with a matched call, per agent.
    pub sessions: BTreeMap<Agent, usize>,
    /// Hook injections found in transcripts, and those after which the agent touched a hinted
    /// file.
    pub hook_acted: Ratio,
}

impl AgentSummary {
    fn from_counts(c: &AgentCounts, scanned: bool) -> AgentSummary {
        let of = |s| Ratio::new(c.count(s), c.answered);
        AgentSummary {
            scanned,
            calls: c.calls,
            answered: c.answered,
            exact: of(Similarity::Exact),
            near: of(Similarity::Near),
            elsewhere: of(Similarity::Elsewhere),
            no_files: of(Similarity::None),
            exact_read: c.exact_action(Action::Read),
            exact_ran: c.exact_action(Action::Ran),
            exact_edited: c.exact_action(Action::Edited),
            first_pick: Ratio::new(c.first_pick, c.count(Similarity::Exact)),
            sessions: c.sessions_by_agent(),
            hook_acted: Ratio::new(c.hook_exact, c.hook_answered),
        }
    }
}

/// The latest `wn bench` history replay: hit@3 per ranker.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Replay {
    /// Commits scored (0 when the bench predates this field).
    pub tasks: usize,
    pub grep_style: f64,
    pub model: f64,
    pub adapter: Option<f64>,
    /// Days since the bench ran.
    pub age_days: u64,
}

/// Stats for one repository, or totals across several.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    /// Repository directory name, or `all repositories`.
    pub name: String,
    /// Repository root (absent for totals).
    pub root: Option<String>,
    pub queries: usize,
    pub abstained: usize,
    /// Queries per kind (`request`, `issue`, `error`, `conversational`).
    pub kinds: BTreeMap<String, usize>,
    /// Queries per day, oldest first.
    pub per_day: Vec<usize>,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    /// Indexed source files.
    pub files: Option<usize>,
    /// Indexed source files per language.
    pub languages: BTreeMap<Language, usize>,
    /// Model name of the latest answer.
    pub model: Option<String>,
    /// Whether the latest answer used the personal adapter.
    pub adapter: bool,
    /// Commits the adapter was fitted on.
    pub adapter_commits: Option<usize>,
    pub agents: AgentSummary,
    /// What the agent hooks (`wn setup`) did.
    pub hooks: HookSummary,
    /// Answers with hints after which you changed a hinted file within a day (git).
    pub you_edited: Ratio,
    pub replay: Option<Replay>,
}

/// Agent hook runs (from `$WHERE_NEXT_HOME/hook-log.jsonl`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Default)]
pub struct HookSummary {
    /// Runs that asked wn (a hook that stays silent before asking is not logged).
    pub runs: usize,
    /// Runs that added hints to the agent's context.
    pub injections: usize,
    /// Files injected.
    pub files: usize,
    /// Sessions with at least one injection.
    pub sessions: usize,
    /// Runs that gave up at the time budget.
    pub timeouts: usize,
    /// Median wall time of a run.
    pub p50_ms: Option<u64>,
    /// Hooks are installed for at least one agent (`wn setup`).
    pub installed: bool,
}

impl HookSummary {
    pub fn of(runs: &[&HookRun]) -> HookSummary {
        let mut ms: Vec<u64> = runs.iter().map(|r| r.ms).collect();
        ms.sort_unstable();
        let injected: Vec<&&HookRun> = runs.iter().filter(|r| r.outcome == "injected").collect();
        let sessions: std::collections::BTreeSet<(&str, &str)> = injected
            .iter()
            .map(|r| (r.agent.as_str(), r.session.as_str()))
            .collect();
        HookSummary {
            runs: runs.len(),
            injections: injected.len(),
            files: injected.iter().map(|r| r.injected.len()).sum(),
            sessions: sessions.len(),
            timeouts: runs.iter().filter(|r| r.outcome == "timeout").count(),
            p50_ms: percentile(&ms, 0.5),
            installed: false,
        }
    }
}

/// Fills in the hook summaries from the hook log (`runs`, already windowed). `installed`: some
/// agent has where-next hooks.
pub fn add_hooks(stats: &mut Stats, runs: &[HookRun], installed: bool) {
    let pick = |root: Option<&str>| -> HookSummary {
        let chosen: Vec<&HookRun> = runs
            .iter()
            .filter(|r| root.map_or(true, |x| same_root(&r.root, Path::new(x))))
            .collect();
        HookSummary {
            installed,
            ..HookSummary::of(&chosen)
        }
    };
    stats.summary.hooks = if stats.scope == "repo" {
        pick(Some(stats.summary.root.as_deref().unwrap_or("")))
    } else {
        pick(None)
    };
    for s in &mut stats.repos {
        s.hooks = pick(Some(s.root.as_deref().unwrap_or("")));
    }
}

/// Everything `wn stats` shows.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Stats {
    /// Window in days.
    pub days: u64,
    /// `repo` or `all`.
    pub scope: String,
    /// The repository, or totals for `--all`.
    pub summary: Summary,
    /// Per repository (`--all` only).
    pub repos: Vec<Summary>,
    /// wn calls in transcripts with no logged answer (logging off, another machine or wn home).
    pub unmatched_calls: usize,
}

/// Display name of a model from its encoder fingerprint (`gemma-xl1-30ae…-model-…` → `gemma-xl1`).
pub fn model_name(fingerprint: &str) -> String {
    if fingerprint.starts_with("hash-") {
        return "lexical fallback".into();
    }
    let parts: Vec<&str> = fingerprint.split('-').collect();
    let end = parts
        .iter()
        .position(|p| p.len() >= 12 && p.chars().all(|c| c.is_ascii_hexdigit()))
        .unwrap_or(parts.len());
    parts[..end.max(1)].join("-")
}

fn windowed(repo: &RepoUsage, from: u64) -> RepoUsage {
    RepoUsage {
        root: repo.root.clone(),
        queries: repo
            .queries
            .iter()
            .filter(|q| q.ts >= from)
            .cloned()
            .collect(),
        index: repo.index.clone(),
        bench: repo.bench.clone(),
    }
}

/// Summarises `repos` over the last `days` days before `now`. `agents` is `None` when transcripts
/// were not read.
pub fn summarize(
    name: &str,
    repos: &[&RepoUsage],
    agents: Option<&AgentScore>,
    now: u64,
    days: u64,
    edited: &EditedFn,
) -> Summary {
    let from = now.saturating_sub(days * DAY);
    let repos: Vec<RepoUsage> = repos.iter().map(|r| windowed(r, from)).collect();
    let mut queries: Vec<_> = repos.iter().flat_map(|r| r.queries.iter()).collect();
    queries.sort_by_key(|q| q.ts);

    let mut per_day = vec![0usize; days as usize];
    let mut kinds = BTreeMap::new();
    let mut ms: Vec<u64> = Vec::new();
    let mut abstained = 0;
    for q in &queries {
        let ago = (now.saturating_sub(q.ts) / DAY) as usize;
        if let Some(slot) = per_day.len().checked_sub(ago + 1) {
            per_day[slot] += 1;
        }
        *kinds.entry(q.kind.clone()).or_insert(0) += 1;
        ms.push(q.ms);
        abstained += usize::from(q.state == "abstain");
    }
    ms.sort_unstable();

    let mut files = None;
    let mut languages = BTreeMap::new();
    for ix in repos.iter().filter_map(|r| r.index.as_ref()) {
        *files.get_or_insert(0) += ix.files;
        for (ext, n) in &ix.extensions {
            *languages.entry(Language::of_extension(ext)).or_insert(0) += n;
        }
    }
    let files = files.or_else(|| {
        repos
            .iter()
            .filter_map(|r| r.queries.last().map(|q| q.files))
            .reduce(|a, b| a + b)
    });

    let last = queries.last();
    let model = last
        .map(|q| q.model.as_str())
        .or_else(|| {
            repos
                .iter()
                .find_map(|r| r.index.as_ref().map(|i| i.model.as_str()))
        })
        .map(model_name);
    let adapter_commits = match repos.as_slice() {
        [one] => one
            .index
            .as_ref()
            .map(|i| i.history_commits)
            .filter(|n| *n > 0),
        _ => None,
    };

    let (mut checked, mut useful) = (0, 0);
    for r in &repos {
        let (c, u) = hint_edits(r, now, edited);
        checked += c;
        useful += u;
    }

    let mut counts = AgentCounts::default();
    if let Some(score) = agents {
        for root in repos.iter().filter_map(|r| r.root.as_ref()) {
            if let Some(c) = score.per_root.get(root) {
                counts.add(c);
            }
        }
    }

    let replay = match repos.as_slice() {
        [one] => one.bench.as_ref().map(|b| Replay {
            tasks: b.tasks,
            grep_style: b.lexical[1],
            model: b.model_hits[1],
            adapter: b.adapter_hits.map(|h| h[1]),
            age_days: now.saturating_sub(b.ts) / DAY,
        }),
        _ => None,
    };

    Summary {
        name: name.to_string(),
        root: match repos.as_slice() {
            [one] => one.root.as_ref().map(|r| r.to_string_lossy().into_owned()),
            _ => None,
        },
        queries: queries.len(),
        abstained,
        kinds,
        per_day,
        p50_ms: percentile(&ms, 0.5),
        p95_ms: percentile(&ms, 0.95),
        files,
        languages,
        model,
        adapter: last.is_some_and(|q| q.adapter),
        adapter_commits,
        agents: AgentSummary::from_counts(&counts, agents.is_some()),
        hooks: HookSummary::default(),
        you_edited: Ratio::new(useful, checked),
        replay,
    }
}

fn repo_name(root: &Path) -> String {
    root.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| root.to_string_lossy().into_owned())
}

fn same_root(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    a == b || canon(a) == canon(b)
}

/// Builds the stats for the repository at `root` (or every repository when `root` is `None`).
/// Returns `None` when nothing is logged for it.
pub fn build(
    usage: &[RepoUsage],
    root: Option<&Path>,
    agents: Option<&AgentScore>,
    now: u64,
    days: u64,
    edited: &EditedFn,
) -> Option<Stats> {
    let days = days.clamp(1, RETENTION_DAYS);
    let unmatched_calls = agents.map_or(0, |a| a.unmatched);
    match root {
        Some(root) => {
            let repo = usage
                .iter()
                .find(|r| r.root.as_deref().is_some_and(|r| same_root(r, root)))?;
            Some(Stats {
                days,
                scope: "repo".into(),
                summary: summarize(&repo_name(root), &[repo], agents, now, days, edited),
                repos: Vec::new(),
                unmatched_calls,
            })
        }
        None => {
            let with_root: Vec<&RepoUsage> = usage.iter().filter(|r| r.root.is_some()).collect();
            if with_root.is_empty() {
                return None;
            }
            let mut repos: Vec<Summary> = with_root
                .iter()
                .map(|r| {
                    let name = repo_name(r.root.as_deref().unwrap_or(Path::new("?")));
                    summarize(&name, &[r], agents, now, days, edited)
                })
                .collect();
            repos.sort_by(|a, b| b.queries.cmp(&a.queries).then(a.name.cmp(&b.name)));
            let name = format!("all repositories ({})", with_root.len());
            Some(Stats {
                days,
                scope: "all".into(),
                summary: summarize(&name, &with_root, agents, now, days, edited),
                repos,
                unmatched_calls,
            })
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Text
// ---------------------------------------------------------------------------------------------

/// Terminal styling (bold only; off for pipes and `NO_COLOR`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Style {
    pub color: bool,
}

impl Style {
    fn bold(self, s: &str) -> String {
        if self.color {
            format!("\x1b[1m{s}\x1b[0m")
        } else {
            s.to_string()
        }
    }
}

/// A 0–1 share as a bar of [`BAR`] cells.
pub fn bar(rate: f64) -> String {
    let full = (rate.clamp(0.0, 1.0) * BAR as f64).round() as usize;
    format!("{}{}", "█".repeat(full), "░".repeat(BAR - full))
}

/// Queries per day as a sparkline (`·` for a day without queries).
pub fn sparkline(per_day: &[usize]) -> String {
    const TICKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = per_day.iter().copied().max().unwrap_or(0).max(1);
    per_day
        .iter()
        .map(|&n| {
            if n == 0 {
                '·'
            } else {
                TICKS[((n * 8).div_ceil(max)).clamp(1, 8) - 1]
            }
        })
        .collect()
}

fn row(label: &str, value: &str) -> String {
    let pad = LABEL.saturating_sub(label.chars().count());
    format!("{label}{}{value}", " ".repeat(pad))
        .trim_end()
        .to_string()
}

fn more(value: &str) -> String {
    row("", value)
}

fn ratio_text(r: Ratio, noun: &str) -> String {
    format!("{} of {} {noun}", r.n, r.of)
}

fn pct(x: f64) -> String {
    format!("{}%", (x * 100.0).round() as u32)
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{n} {}", if n == 1 { one } else { many })
}

fn sessions_text(sessions: &BTreeMap<Agent, usize>) -> String {
    let parts: Vec<String> = sessions
        .iter()
        .map(|(a, n)| format!("{n} {}", a.label()))
        .collect();
    let total: usize = sessions.values().sum();
    format!(
        "from {} {}",
        parts.join(" / "),
        if total == 1 { "session" } else { "sessions" }
    )
}

/// How long `wn bench` takes on 300 commits: 18 s on ripgrep with gemma-xl1 on an Apple M-series
/// laptop once the files are embedded; the first run on a big repository embeds past file
/// versions and takes minutes.
pub const BENCH_ESTIMATE: &str = "~20 s to a few min for 300 commits";

fn exact_detail(a: &AgentSummary) -> String {
    let mut parts = vec![format!(
        "on the hinted file: read {} · ran {} · edited {}",
        a.exact_read, a.exact_ran, a.exact_edited
    )];
    if a.first_pick.of > 0 {
        parts.push(format!(
            "took #1 first {} of {}",
            a.first_pick.n, a.first_pick.of
        ));
    }
    parts.join(" · ")
}

fn agent_rows(a: &AgentSummary, style: Style) -> Vec<String> {
    const LABEL_AGENTS: &str = "Agents used the hint";
    if !a.scanned {
        return vec![row(LABEL_AGENTS, "not checked (--no-agents)")];
    }
    if a.calls == 0 {
        return vec![row(LABEL_AGENTS, "no agent calls found yet")];
    }
    if a.answered == 0 {
        return vec![
            row(
                LABEL_AGENTS,
                &format!(
                    "{} with no hints (abstained)",
                    plural(a.calls, "call", "calls")
                ),
            ),
            more(&sessions_text(&a.sessions)),
        ];
    }
    let head = match (a.exact.pct(), a.near.pct()) {
        (Some(e), Some(n)) => format!(
            "{}  {} exact · {n}% near  ({})",
            bar(a.exact.rate().unwrap_or(0.0)),
            style.bold(&format!("{e}%")),
            plural(a.answered, "answer", "answers")
        ),
        _ => format!("exact {} of {} · near {}", a.exact.n, a.answered, a.near.n),
    };
    vec![
        row(LABEL_AGENTS, &head),
        more(&exact_detail(a)),
        more(&format!(
            "elsewhere {} · no files {} · {}",
            a.elsewhere.n,
            a.no_files.n,
            sessions_text(&a.sessions)
        )),
    ]
}

fn replay_value(r: &Replay) -> String {
    let mut parts = Vec::new();
    if let Some(a) = r.adapter {
        parts.push(format!("wn + adapter {}", pct(a)));
    }
    parts.push(format!("wn {}", pct(r.model)));
    parts.push(format!("grep-style {}", pct(r.grep_style)));
    let when = match r.age_days {
        0 => "today".to_string(),
        1 => "yesterday".to_string(),
        n => format!("{n} days ago"),
    };
    let on = if r.tasks > 0 {
        format!("{} commits, {when}", r.tasks)
    } else {
        when
    };
    format!("{}  ({on})", parts.join(" · "))
}

fn speed_value(s: &Summary) -> Option<String> {
    let mut parts = Vec::new();
    if let (Some(p50), Some(p95)) = (s.p50_ms, s.p95_ms) {
        parts.push(format!("p50 {p50} ms · p95 {p95} ms"));
    }
    if let Some(f) = s.files {
        parts.push(plural(f, "file", "files"));
    }
    (!parts.is_empty()).then(|| parts.join(" · "))
}

fn queries_value(s: &Summary) -> String {
    if s.queries == 0 {
        return "0 → try `wn ask \"where is …\"`".into();
    }
    let mut kinds: Vec<(&String, &usize)> = s.kinds.iter().collect();
    kinds.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    let mut parts: Vec<String> = kinds.iter().map(|(k, n)| format!("{k} {n}")).collect();
    parts.push(format!("abstained {}", s.abstained));
    format!(
        "{}  {}  {}",
        s.queries,
        sparkline(&s.per_day),
        parts.join(" · ")
    )
}

fn model_value(s: &Summary) -> Option<String> {
    let m = s.model.as_ref()?;
    Some(match (s.adapter, s.adapter_commits) {
        (true, Some(n)) => format!("{m} + adapter ({n} commits)"),
        (true, None) => format!("{m} + adapter"),
        (false, _) => m.clone(),
    })
}

/// The line under the title: what the hooks did, or how to install them.
pub fn hooks_line(s: &Summary) -> String {
    let h = &s.hooks;
    if h.runs == 0 {
        return if h.installed {
            "hooks: installed; nothing injected yet (they answer in repositories indexed with `wn init`)"
                .into()
        } else {
            "hooks: not installed → `wn setup` connects Claude Code, Codex and Cursor so hints arrive automatically"
                .into()
        };
    }
    let mut line = format!(
        "hooks: {} in {}",
        plural(h.injections, "injection", "injections"),
        plural(h.sessions, "session", "sessions")
    );
    let acted = s.agents.hook_acted;
    if s.agents.scanned && acted.of > 0 {
        line.push_str(&match acted.pct() {
            Some(p) => format!(
                " · the agent then opened a hinted file after {p}% ({} of {})",
                acted.n, acted.of
            ),
            None => format!(
                " · the agent then opened a hinted file after {} of {}",
                acted.n, acted.of
            ),
        });
    }
    if !h.installed {
        line.push_str(" · not installed now (`wn setup`)");
    }
    line
}

fn hook_rows(s: &Summary) -> Vec<String> {
    let h = &s.hooks;
    if h.runs == 0 {
        return Vec::new();
    }
    let quiet = h.runs - h.injections - h.timeouts;
    let mut parts = vec![plural(h.files, "file injected", "files injected")];
    if let Some(ms) = h.p50_ms {
        parts.push(format!("median {ms} ms"));
    }
    parts.push(format!("quiet {quiet} · timed out {}", h.timeouts));
    vec![row("Hooks", &parts.join(" · "))]
}

fn summary_rows(s: &Summary, style: Style, with_replay: bool) -> Vec<String> {
    let mut lines = agent_rows(&s.agents, style);
    lines.extend(hook_rows(s));
    lines.push(row(
        "You edited a hint",
        &if s.you_edited.of == 0 {
            "no answers with hints yet".to_string()
        } else {
            format!("{} within 24 h (git)", ratio_text(s.you_edited, "answers"))
        },
    ));
    if with_replay {
        lines.push(match &s.replay {
            Some(r) => row("Replay (top 3)", &replay_value(r)),
            None => row(
                "Replay (top 3)",
                &format!("not run yet → `wn bench` ({BENCH_ESTIMATE})"),
            ),
        });
    }
    if let Some(v) = speed_value(s) {
        lines.push(row("Speed", &v));
    }
    lines.push(row("Queries", &queries_value(s)));
    if let Some(v) = model_value(s) {
        lines.push(row("Model", &v));
    }
    lines
}

fn ratio_cell(r: Ratio) -> String {
    match (r.of, r.pct()) {
        (0, _) => "–".into(),
        (_, Some(p)) => format!("{}/{} {p}%", r.n, r.of),
        (_, None) => format!("{} of {}", r.n, r.of),
    }
}

fn table(repos: &[Summary]) -> Vec<String> {
    let header = [
        "repo",
        "queries",
        "agent exact",
        "you edited",
        "replay@3",
        "p50",
    ];
    let rows: Vec<[String; 6]> = repos
        .iter()
        .map(|s| {
            [
                s.name.clone(),
                s.queries.to_string(),
                if s.agents.scanned {
                    ratio_cell(s.agents.exact)
                } else {
                    "–".into()
                },
                ratio_cell(s.you_edited),
                s.replay
                    .as_ref()
                    .map_or("–".into(), |r| pct(r.adapter.unwrap_or(r.model))),
                s.p50_ms.map_or("–".into(), |m| format!("{m} ms")),
            ]
        })
        .collect();
    let width = |i: usize| {
        rows.iter()
            .map(|r| r[i].chars().count())
            .chain([header[i].len()])
            .max()
            .unwrap_or(0)
    };
    let widths: Vec<usize> = (0..6).map(width).collect();
    let fmt = |cells: [&str; 6]| {
        cells
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let pad = " ".repeat(widths[i] - c.chars().count());
                if i == 0 {
                    format!("{c}{pad}")
                } else {
                    format!("{pad}{c}")
                }
            })
            .collect::<Vec<_>>()
            .join("   ")
            .trim_end()
            .to_string()
    };
    let mut out = vec![fmt(header)];
    for r in &rows {
        out.push(fmt([&r[0], &r[1], &r[2], &r[3], &r[4], &r[5]]));
    }
    out
}

/// Text form of the stats.
pub fn render(stats: &Stats, style: Style) -> String {
    let title = style.bold(&format!(
        "wn stats · {} · last {}",
        stats.summary.name,
        plural(stats.days as usize, "day", "days")
    ));
    let mut lines = vec![title, hooks_line(&stats.summary), String::new()];
    let all = stats.scope == "all";
    lines.extend(summary_rows(&stats.summary, style, !all));
    if all {
        lines.push(String::new());
        lines.extend(table(&stats.repos));
    }
    if stats.unmatched_calls > 0 {
        lines.push(String::new());
        lines.push(format!(
            "{} in agent transcripts had no logged answer (logging off, or another wn home)",
            plural(stats.unmatched_calls, "wn call", "wn calls")
        ));
    }
    lines.join("\n")
}

// ---------------------------------------------------------------------------------------------
// Sharing
// ---------------------------------------------------------------------------------------------

/// Redacted stats for posting: numbers and allowlisted enums only. Nothing here can carry a
/// repository name, path, remote, query or file name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShareCard {
    pub days: u64,
    pub repos: usize,
    /// Indexed source files, rounded to one significant figure.
    pub files_approx: Option<u64>,
    /// Most common language of indexed source files.
    pub language: Option<Language>,
    pub model: KnownModel,
    pub adapter: bool,
    /// Answers with hints after which the agent touched a hinted file / a file next to one /
    /// other files / no files (they partition the answers).
    pub agents_exact: Ratio,
    pub agents_near: Ratio,
    pub agents_elsewhere: Ratio,
    pub agents_no_files: Ratio,
    /// Exact answers by the strongest action on a hinted file.
    pub agents_exact_read: usize,
    pub agents_exact_ran: usize,
    pub agents_exact_edited: usize,
    pub agents_first_pick: Ratio,
    pub agent_sessions: usize,
    pub you_edited: Ratio,
    /// hit@3: `[grep-style, wn, wn + adapter]`.
    pub replay: Option<[Option<f64>; 3]>,
    pub replay_commits: usize,
    pub queries: usize,
    pub p50_ms: Option<u32>,
}

/// Rounds to one significant figure (`294` → `300`, `1_430` → `1_000`).
pub fn one_sig_fig(n: u64) -> u64 {
    if n < 10 {
        return n;
    }
    let mag = 10u64.pow(n.ilog10());
    ((n + mag / 2) / mag) * mag
}

fn language_name(l: Language) -> &'static str {
    match l {
        Language::Python => "Python",
        Language::Rust => "Rust",
        Language::Go => "Go",
        Language::Typescript => "TypeScript",
        Language::Javascript => "JavaScript",
        Language::Java => "Java",
        Language::Kotlin => "Kotlin",
        Language::C => "C",
        Language::Cpp => "C++",
        Language::Csharp => "C#",
        Language::Ruby => "Ruby",
        Language::Php => "PHP",
        Language::Swift => "Swift",
        Language::Scala => "Scala",
        Language::Shell => "shell",
        Language::Other => "mixed",
    }
}

fn model_label(m: KnownModel) -> &'static str {
    match m {
        KnownModel::GemmaXl1 => "gemma-xl1",
        KnownModel::GemmaG2r => "gemma-g2r",
        KnownModel::GemmaG1 => "gemma-g1",
        KnownModel::V2b => "v2b",
        KnownModel::Custom => "custom model",
        KnownModel::Lexical => "lexical fallback",
    }
}

impl ShareCard {
    /// Redacts a summary.
    pub fn from_stats(stats: &Stats) -> ShareCard {
        let s = &stats.summary;
        let language = s
            .languages
            .iter()
            .filter(|(l, _)| **l != Language::Other)
            .max_by_key(|(_, n)| **n)
            .or_else(|| s.languages.iter().max_by_key(|(_, n)| **n))
            .map(|(l, _)| *l);
        ShareCard {
            days: stats.days,
            repos: if stats.scope == "all" {
                stats.repos.len()
            } else {
                1
            },
            files_approx: s.files.map(|f| one_sig_fig(f as u64)),
            language,
            model: s.model.as_deref().map_or(KnownModel::Lexical, |m| {
                if m == "lexical fallback" {
                    KnownModel::Lexical
                } else {
                    KnownModel::from_name(m)
                }
            }),
            adapter: s.adapter,
            agents_exact: s.agents.exact,
            agents_near: s.agents.near,
            agents_elsewhere: s.agents.elsewhere,
            agents_no_files: s.agents.no_files,
            agents_exact_read: s.agents.exact_read,
            agents_exact_ran: s.agents.exact_ran,
            agents_exact_edited: s.agents.exact_edited,
            agents_first_pick: s.agents.first_pick,
            agent_sessions: s.agents.sessions.values().sum(),
            you_edited: s.you_edited,
            replay: s
                .replay
                .as_ref()
                .map(|r| [Some(r.grep_style), Some(r.model), r.adapter]),
            replay_commits: s.replay.as_ref().map_or(0, |r| r.tasks),
            queries: s.queries,
            p50_ms: s.p50_ms.map(round_ms),
        }
    }

    /// "a ~300-file Python repo", or "3 repos, mostly Rust (~2000 files)".
    pub fn describe(&self) -> String {
        let lang = self.language.map(language_name);
        match (self.repos, self.files_approx, lang) {
            (1, Some(f), Some(l)) => format!("a ~{f}-file {l} repo"),
            (1, Some(f), None) => format!("a ~{f}-file repo"),
            (1, None, _) => "one repo".into(),
            (n, f, l) => {
                let mut s = format!("{n} repos");
                if let Some(l) = l {
                    s.push_str(&format!(", mostly {l}"));
                }
                if let Some(f) = f {
                    s.push_str(&format!(" (~{f} files)"));
                }
                s
            }
        }
    }

    fn model_text(&self) -> String {
        let m = model_label(self.model);
        if self.adapter {
            format!("{m} + adapter")
        } else {
            m.to_string()
        }
    }
}

/// Text form of the share card.
pub fn render_share(c: &ShareCard) -> String {
    let mut lines = vec![
        format!(
            "where-next · {} · last {}",
            c.describe(),
            plural(c.days as usize, "day", "days")
        ),
        String::new(),
    ];
    let a = c.agents_exact;
    if a.of > 0 {
        lines.push(row(
            "Agents used the hint",
            &match (a.pct(), c.agents_near.pct()) {
                (Some(e), Some(n)) => format!(
                    "{}  {e}% exact · {n}% near  ({})",
                    bar(a.rate().unwrap_or(0.0)),
                    plural(a.of, "answer", "answers")
                ),
                _ => format!(
                    "exact {} of {} · near {} · elsewhere {} · no files {}",
                    a.n, a.of, c.agents_near.n, c.agents_elsewhere.n, c.agents_no_files.n
                ),
            },
        ));
        let mut detail = vec![format!(
            "on the hinted file: read {} · ran {} · edited {}",
            c.agents_exact_read, c.agents_exact_ran, c.agents_exact_edited
        )];
        if c.agents_first_pick.of > 0 {
            detail.push(format!(
                "took #1 first {} of {}",
                c.agents_first_pick.n, c.agents_first_pick.of
            ));
        }
        lines.push(more(&detail.join(" · ")));
    }
    if let Some([grep, model, adapter]) = c.replay {
        let mut parts = Vec::new();
        if let Some(x) = adapter {
            parts.push(format!("wn + adapter {}", pct(x)));
        }
        if let Some(x) = model {
            parts.push(format!("wn {}", pct(x)));
        }
        if let Some(x) = grep {
            parts.push(format!("grep-style {}", pct(x)));
        }
        let on = if c.replay_commits > 0 {
            format!("  ({} past commits)", c.replay_commits)
        } else {
            String::new()
        };
        lines.push(row(
            "Right file in top 3",
            &format!("{}{on}", parts.join(" · ")),
        ));
    }
    if c.you_edited.of > 0 {
        lines.push(row(
            "You edited a hint",
            &ratio_text(c.you_edited, "answers"),
        ));
    }
    let mut speed = Vec::new();
    if let Some(ms) = c.p50_ms {
        speed.push(format!("{ms} ms median"));
    }
    speed.push(plural(c.queries, "query", "queries"));
    speed.push(c.model_text());
    lines.push(row("Speed", &speed.join(" · ")));
    lines.join("\n")
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The share card as a self-contained SVG (one deliberate dark look, system fonts, no external
/// resources). Rendered from a [`ShareCard`] only, so it cannot contain un-redacted data.
pub fn render_svg(c: &ShareCard) -> String {
    const W: u32 = 640;
    const INK: &str = "#0e1116";
    const EDGE: &str = "#262c36";
    const TEXT: &str = "#e8ecf1";
    const MUTED: &str = "#8a94a3";
    const EXACT: &str = "#4fd1b5";
    const NEAR: &str = "#2f8f7d";
    const OTHER: &str = "#5b6573";
    const NONE: &str = "#2a313b";
    const MODEL: &str = "#7aa2c8";
    const TRACK: &str = "#1f252e";
    const FONT: &str =
        "ui-sans-serif, -apple-system, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif";
    const MONO: &str = "ui-monospace, 'SF Mono', Menlo, Consolas, monospace";

    struct T<'a> {
        x: u32,
        y: u32,
        size: u32,
        fill: &'a str,
        weight: u32,
        anchor: &'a str,
        font: &'a str,
    }
    fn text(t: T, s: &str) -> String {
        format!(
            r#"<text x="{}" y="{}" font-family="{}" font-size="{}" font-weight="{}" fill="{}" text-anchor="{}">{}</text>"#,
            t.x,
            t.y,
            t.font,
            t.size,
            t.weight,
            t.fill,
            t.anchor,
            xml(s)
        )
    }
    let t = |x, y, size, fill, weight| T {
        x,
        y,
        size,
        fill,
        weight,
        anchor: "start",
        font: FONT,
    };

    let mut body = Vec::new();
    let mut y = 44;
    body.push(text(t(32, y, 13, MUTED, 600), "WHERE-NEXT"));
    body.push(text(
        T {
            anchor: "end",
            ..t(W - 32, y, 13, MUTED, 400)
        },
        &format!("last {}", plural(c.days as usize, "day", "days")),
    ));
    body.push(text(t(32, 70, 16, TEXT, 500), &c.describe()));
    y = 140;

    let agents = c.agents_exact;
    if agents.of > 0 {
        // Headline: how often the agent touched a hinted file, and what else it touched.
        let big = a_pct(agents);
        body.push(text(t(32, y, 56, EXACT, 700), &big));
        let cap_x = 32 + text_width(&big, 56) + 18;
        body.push(text(
            t(cap_x, y - 22, 15, TEXT, 500),
            "of answers led the agent to a hinted file",
        ));
        let mut sub = vec![format!(
            "read {} · ran {} · edited {}",
            c.agents_exact_read, c.agents_exact_ran, c.agents_exact_edited
        )];
        if c.agents_first_pick.of > 0 {
            sub.push(format!(
                "#1 first {} of {}",
                c.agents_first_pick.n, c.agents_first_pick.of
            ));
        }
        body.push(text(t(cap_x, y, 13, MUTED, 400), &sub.join(" · ")));
        // The partition of answers as one stacked bar, with a legend.
        let parts = [
            ("exact", c.agents_exact.n, EXACT),
            ("near", c.agents_near.n, NEAR),
            ("elsewhere", c.agents_elsewhere.n, OTHER),
            ("no files", c.agents_no_files.n, NONE),
        ];
        let total = agents.of.max(1) as f64;
        let width = f64::from(W - 64);
        let mut x = 32.0;
        y += 20;
        body.push(format!(
            r#"<clipPath id="wn-split"><rect x="32" y="{y}" width="{}" height="10" rx="5"/></clipPath>"#,
            W - 64
        ));
        let mut segs = vec![format!(
            r#"<rect x="32" y="{y}" width="{}" height="10" fill="{TRACK}"/>"#,
            W - 64
        )];
        for (_, n, col) in parts {
            let w = n as f64 / total * width;
            if w > 0.0 {
                segs.push(format!(
                    r#"<rect x="{x:.1}" y="{y}" width="{w:.1}" height="10" fill="{col}"/>"#
                ));
            }
            x += w;
        }
        body.push(format!(
            r#"<g clip-path="url(#wn-split)">{}</g>"#,
            segs.concat()
        ));
        y += 30;
        let mut lx = 32;
        for (label, n, col) in parts {
            body.push(format!(
                r#"<circle cx="{}" cy="{}" r="4" fill="{col}"/>"#,
                lx + 4,
                y - 4
            ));
            let s = format!("{label} {n}");
            body.push(text(t(lx + 14, y, 12, MUTED, 400), &s));
            lx += 14 + text_width(&s, 12) + 20;
        }
        body.push(text(
            T {
                anchor: "end",
                ..t(W - 32, y, 12, MUTED, 400)
            },
            &format!(
                "{} · {}",
                plural(agents.of, "answer", "answers"),
                plural(c.agent_sessions, "session", "sessions")
            ),
        ));
        y += 38;
    } else {
        let (big, caption) = match c.replay {
            Some([_, Some(m), a]) => (
                pct(a.unwrap_or(m)),
                "right file in the top 3 on past commits",
            ),
            _ => (
                c.p50_ms.map_or("–".into(), |m| format!("{m} ms")),
                "median answer time",
            ),
        };
        body.push(text(t(32, y, 56, EXACT, 700), &big));
        let cap_x = 32 + text_width(&big, 56) + 18;
        body.push(text(t(cap_x, y - 22, 15, TEXT, 500), caption));
        body.push(text(
            t(cap_x, y, 13, MUTED, 400),
            &plural(c.queries, "query", "queries"),
        ));
        y += 44;
    }

    if let Some([grep, model, adapter]) = c.replay {
        let label = if c.replay_commits > 0 {
            format!(
                "Right file in top 3 · replayed on {} past commits",
                c.replay_commits
            )
        } else {
            "Right file in top 3 · replayed on past commits".to_string()
        };
        body.push(text(t(32, y, 12, MUTED, 600), &label));
        y += 12;
        let rows = [
            ("wn + adapter", adapter, EXACT),
            ("wn", model, MODEL),
            ("grep-style", grep, OTHER),
        ];
        for (label, v, col) in rows {
            let Some(v) = v else { continue };
            let track = 360u32;
            let fill = (v.clamp(0.0, 1.0) * f64::from(track)).round() as u32;
            body.push(text(t(32, y + 11, 13, TEXT, 400), label));
            body.push(format!(
                r#"<rect x="150" y="{y}" width="{track}" height="12" rx="6" fill="{TRACK}"/><rect x="150" y="{y}" width="{fill}" height="12" rx="6" fill="{col}"/>"#
            ));
            body.push(text(
                T {
                    font: MONO,
                    ..t(526, y + 11, 13, TEXT, 600)
                },
                &pct(v),
            ));
            y += 22;
        }
        y += 10;
    }

    let mut foot = Vec::new();
    if let Some(ms) = c.p50_ms {
        foot.push(format!("{ms} ms median"));
    }
    foot.push(c.model_text());
    if c.you_edited.of > 0 {
        foot.push(format!(
            "you edited {} of {} hints",
            c.you_edited.n, c.you_edited.of
        ));
    }
    body.push(format!(
        r#"<line x1="32" y1="{y}" x2="{}" y2="{y}" stroke="{EDGE}"/>"#,
        W - 32
    ));
    y += 22;
    body.push(text(t(32, y, 12, MUTED, 400), &foot.join(" · ")));
    body.push(text(
        T {
            anchor: "end",
            ..t(W - 32, y, 12, MUTED, 600)
        },
        "where-next · wn",
    ));
    let h = y + 20;

    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{h}" viewBox="0 0 {W} {h}" role="img" aria-label="where-next stats: {}">
<rect x="0.5" y="0.5" width="{}" height="{}" rx="14" fill="{INK}" stroke="{EDGE}"/>
{}
</svg>
"#,
        xml(&c.describe()),
        W - 1,
        h - 1,
        body.join("\n")
    )
}

/// Approximate advance width of `s` in a proportional sans-serif at `size` px.
fn text_width(s: &str, size: u32) -> u32 {
    let em: f64 = s
        .chars()
        .map(|c| match c {
            '0'..='9' => 0.62,
            '%' => 0.92,
            ' ' => 0.28,
            '/' | '.' | '·' => 0.36,
            'm' | 'w' => 0.85,
            c if c.is_uppercase() => 0.68,
            _ => 0.52,
        })
        .sum();
    (em * f64::from(size)).round() as u32
}

/// A ratio as a percentage when there are enough answers, else "n/m".
fn a_pct(r: Ratio) -> String {
    r.pct()
        .map_or_else(|| format!("{}/{}", r.n, r.of), |p| format!("{p}%"))
}

/// Where usage was recorded for `path`'s repository, as the cache keys it.
pub fn repo_root_of(path: &Path) -> PathBuf {
    let root = wn_git::repo_root(path);
    root.canonicalize().unwrap_or(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_names() {
        assert_eq!(
            model_name("gemma-xl1-30ae960f08a8d9e8-model-wn-sources-v1"),
            "gemma-xl1"
        );
        assert_eq!(model_name("hash-abc"), "lexical fallback");
        assert_eq!(model_name("custom"), "custom");
    }

    #[test]
    fn sig_figs() {
        assert_eq!(one_sig_fig(7), 7);
        assert_eq!(one_sig_fig(294), 300);
        assert_eq!(one_sig_fig(1_430), 1_000);
        assert_eq!(one_sig_fig(31_000), 30_000);
        assert_eq!(one_sig_fig(96), 100);
    }

    #[test]
    fn small_samples_have_no_percent() {
        assert_eq!(Ratio::new(3, 4).rate(), None);
        assert_eq!(Ratio::new(3, 5).pct(), Some(60));
        assert_eq!(Ratio::new(9, 5).n, 5, "clamped");
    }

    #[test]
    fn sparkline_marks_empty_days() {
        assert_eq!(sparkline(&[0, 1, 8, 4]), "·▁█▄");
        assert_eq!(sparkline(&[]), "");
    }

    #[test]
    fn bars_are_fixed_width() {
        for r in [0.0, 0.33, 0.7, 1.0, 2.0, -1.0] {
            assert_eq!(bar(r).chars().count(), BAR);
        }
    }
}
