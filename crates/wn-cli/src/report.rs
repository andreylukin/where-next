//! `wn report`: an opt-in, privacy-safe usage report the user reads in full and posts themselves.
//!
//! The report is built only from the local usage log ([`wn_daemon::usage`]) and reduced to an
//! allowlist of numbers, fixed enums and buckets. Its types have no field that can carry text
//! from a repository: no paths, file names, repository names, remotes, commit messages, queries,
//! user names or host names. Nothing is sent unless the user confirms, and then only through a
//! GitHub issue they submit from their own account (the browser, or `gh` when it is installed).
//!
//! The flow is an explicit state machine: Collecting → Previewing → Confirmed | Cancelled →
//! Posting → Posted | Failed.

use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::process::Command as Process;

use serde::{Deserialize, Serialize};
use wn_core::machine::{step, Illegal};
use wn_daemon::usage::{load_all, RepoUsage};

/// Report schema version.
pub const SCHEMA: u32 = 1;
/// The repository reports go to.
pub const REPO: &str = "andreylukin/where-next";
/// Longest pre-filled issue URL we open.
pub const MAX_URL: usize = 8_000;
/// Marker the aggregation Action looks for.
pub const MARKER: &str = "<!-- wn-usage-report v1 -->";
/// Issue title.
pub const TITLE: &str = "usage report: wn";
/// Most recent query events per repository checked for later edits.
pub const MAX_CHECKED: usize = 200;
/// A hint counts as useful when its file changes within this window after the answer.
pub const USEFUL_WINDOW_S: u64 = 86_400;
/// At most this many bench summaries (one per repository) go into a report.
pub const MAX_BENCH: usize = 10;

// ---------------------------------------------------------------------------------------------
// Report types: numbers, fixed enums and buckets only
// ---------------------------------------------------------------------------------------------

/// A version string of digits and dots (the crate version).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Version(String);

impl TryFrom<String> for Version {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let ok =
            !s.is_empty() && s.len() <= 20 && s.chars().all(|c| c.is_ascii_digit() || c == '.');
        ok.then_some(Version(s))
            .ok_or_else(|| "version must be digits and dots".into())
    }
}

impl From<Version> for String {
    fn from(v: Version) -> String {
        v.0
    }
}

/// A short commit hash of the `wn` build (7–12 lowercase hex digits).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct BuildCommit(String);

impl TryFrom<String> for BuildCommit {
    type Error = String;
    fn try_from(s: String) -> Result<Self, String> {
        let ok = (7..=12).contains(&s.len())
            && s.chars()
                .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c));
        ok.then_some(BuildCommit(s))
            .ok_or_else(|| "commit must be 7-12 lowercase hex digits".into())
    }
}

impl From<BuildCommit> for String {
    fn from(v: BuildCommit) -> String {
        v.0
    }
}

/// The `wn` build.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WnBuild {
    pub version: Version,
    pub commit: Option<BuildCommit>,
}

/// Operating system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Os {
    Macos,
    Linux,
    Windows,
    Other,
}

/// CPU architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Arch {
    Aarch64,
    #[serde(rename = "x86_64")]
    X86_64,
    Other,
}

/// Hardware threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Threads {
    #[serde(rename = "1-4")]
    UpTo4,
    #[serde(rename = "5-8")]
    UpTo8,
    #[serde(rename = "9-16")]
    UpTo16,
    #[serde(rename = "17+")]
    More,
}

/// Installed memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ram {
    #[serde(rename = "<8GB")]
    Under8,
    #[serde(rename = "8-16GB")]
    To16,
    #[serde(rename = "16-32GB")]
    To32,
    #[serde(rename = "32-64GB")]
    To64,
    #[serde(rename = "64GB+")]
    More,
    #[serde(rename = "unknown")]
    Unknown,
}

/// A model from the published list, a custom one, or the lexical fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum KnownModel {
    GemmaXl1,
    GemmaG2r,
    GemmaG1,
    V2b,
    Custom,
    Lexical,
}

impl KnownModel {
    /// Maps an encoder fingerprint or model directory name to a known model.
    pub fn from_name(name: &str) -> KnownModel {
        const KNOWN: [(&str, KnownModel); 4] = [
            ("gemma-xl1", KnownModel::GemmaXl1),
            ("gemma-g2r", KnownModel::GemmaG2r),
            ("gemma-g1", KnownModel::GemmaG1),
            ("v2b", KnownModel::V2b),
        ];
        if name.starts_with("hash-") {
            return KnownModel::Lexical;
        }
        KNOWN
            .iter()
            .find(|(k, _)| name == *k || name.starts_with(&format!("{k}-")))
            .map(|(_, m)| *m)
            .unwrap_or(KnownModel::Custom)
    }
}

/// Model graph precision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Precision {
    Fp32,
    Q8,
    None,
}

/// Machine and model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Setup {
    pub os: Os,
    pub arch: Arch,
    pub threads: Threads,
    pub ram: Ram,
    pub model: KnownModel,
    pub precision: Precision,
}

/// Indexed source files in a repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Files {
    #[serde(rename = "<1k")]
    Under1k,
    #[serde(rename = "1k-3k")]
    To3k,
    #[serde(rename = "3k-10k")]
    To10k,
    #[serde(rename = "10k+")]
    More,
}

impl Files {
    pub fn of(n: usize) -> Files {
        match n {
            0..=999 => Files::Under1k,
            1000..=2999 => Files::To3k,
            3000..=9999 => Files::To10k,
            _ => Files::More,
        }
    }
}

/// A count, bucketed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Count {
    #[serde(rename = "0")]
    Zero,
    #[serde(rename = "1-9")]
    Few,
    #[serde(rename = "10-49")]
    Some,
    #[serde(rename = "50-199")]
    Many,
    #[serde(rename = "200+")]
    Lots,
}

impl Count {
    pub fn of(n: usize) -> Count {
        match n {
            0 => Count::Zero,
            1..=9 => Count::Few,
            10..=49 => Count::Some,
            50..=199 => Count::Many,
            _ => Count::Lots,
        }
    }
}

/// Commits the personal adapter was fitted on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum History {
    #[serde(rename = "none")]
    None,
    #[serde(rename = "<50")]
    Under50,
    #[serde(rename = "50-199")]
    To200,
    #[serde(rename = "200+")]
    More,
}

impl History {
    pub fn of(n: usize) -> History {
        match n {
            0 => History::None,
            1..=49 => History::Under50,
            50..=199 => History::To200,
            _ => History::More,
        }
    }
}

/// Languages of indexed source files (by extension).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Language {
    Python,
    Rust,
    Go,
    Typescript,
    Javascript,
    Java,
    Kotlin,
    C,
    Cpp,
    Csharp,
    Ruby,
    Php,
    Swift,
    Scala,
    Shell,
    Other,
}

impl Language {
    pub fn of_extension(ext: &str) -> Language {
        match ext.to_ascii_lowercase().as_str() {
            "py" | "pyi" => Language::Python,
            "rs" => Language::Rust,
            "go" => Language::Go,
            "ts" | "tsx" | "mts" | "cts" => Language::Typescript,
            "js" | "jsx" | "mjs" | "cjs" => Language::Javascript,
            "java" => Language::Java,
            "kt" | "kts" => Language::Kotlin,
            "c" | "h" => Language::C,
            "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => Language::Cpp,
            "cs" => Language::Csharp,
            "rb" => Language::Ruby,
            "php" => Language::Php,
            "swift" => Language::Swift,
            "scala" | "sc" => Language::Scala,
            "sh" | "bash" | "zsh" => Language::Shell,
            _ => Language::Other,
        }
    }
}

/// The repositories wn was used in (aggregated; no identifiers).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Repos {
    /// Repositories with logged usage.
    pub count: Count,
    /// Repositories per size bucket.
    pub sizes: BTreeMap<Files, Count>,
    /// Share of indexed source files per language, in percent rounded to 10 (below 10 omitted).
    pub languages: BTreeMap<Language, u8>,
    /// Repositories per adapter-history bucket.
    pub history: BTreeMap<History, Count>,
}

/// Wall time of the latest index build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum IndexTime {
    #[serde(rename = "<10s")]
    Under10s,
    #[serde(rename = "10-60s")]
    ToMinute,
    #[serde(rename = "1-5min")]
    To5Min,
    #[serde(rename = "5min+")]
    More,
}

impl IndexTime {
    pub fn of_ms(ms: u64) -> IndexTime {
        match ms {
            0..=9_999 => IndexTime::Under10s,
            10_000..=59_999 => IndexTime::ToMinute,
            60_000..=299_999 => IndexTime::To5Min,
            _ => IndexTime::More,
        }
    }
}

/// Peak memory of `wn init`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Memory {
    #[serde(rename = "<512MB")]
    Under512,
    #[serde(rename = "512MB-1GB")]
    To1G,
    #[serde(rename = "1-2GB")]
    To2G,
    #[serde(rename = "2-4GB")]
    To4G,
    #[serde(rename = "4GB+")]
    More,
}

impl Memory {
    pub fn of_mb(mb: u64) -> Memory {
        match mb {
            0..=511 => Memory::Under512,
            512..=1023 => Memory::To1G,
            1024..=2047 => Memory::To2G,
            2048..=4095 => Memory::To4G,
            _ => Memory::More,
        }
    }
}

/// Speed and memory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Performance {
    /// Median index build time across repositories.
    pub index_time: Option<IndexTime>,
    /// Median answer latency (ms, rounded).
    pub query_ms_p50: Option<u32>,
    /// 95th-percentile answer latency (ms, rounded).
    pub query_ms_p95: Option<u32>,
    /// Largest peak memory seen while indexing.
    pub peak_memory: Option<Memory>,
}

/// hit@1, hit@3 and hit@10 of one `wn bench --history` run (rounded to 0.01).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchSummary {
    /// Candidate-set size of the repository.
    pub files: Files,
    /// The model the bench ran with.
    pub model: KnownModel,
    pub lexical: [f64; 3],
    pub model_hits: [f64; 3],
    pub adapter_hits: Option<[f64; 3]>,
}

/// Answer quality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quality {
    /// Latest bench per repository, sorted, at most [`MAX_BENCH`].
    pub bench: Vec<BenchSummary>,
    /// Share of queries that abstained (rounded to 0.05).
    pub abstain_rate: Option<f64>,
    /// Share of answers whose hinted files were edited within a day (rounded to 0.05).
    pub hint_usefulness: Option<f64>,
    /// Answers checked for later edits.
    pub hints_checked: Count,
}

/// Non-ok answer states.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateCode {
    Abstain,
    EmptyIndex,
    UnsupportedScope,
    StaleIndex,
    Error,
    Other,
}

impl StateCode {
    fn of(label: &str) -> Option<StateCode> {
        match label {
            "ok" => None,
            "abstain" => Some(StateCode::Abstain),
            "empty_index" => Some(StateCode::EmptyIndex),
            "unsupported_scope" => Some(StateCode::UnsupportedScope),
            "stale_index" => Some(StateCode::StaleIndex),
            "error" => Some(StateCode::Error),
            _ => Some(StateCode::Other),
        }
    }
}

/// Failures and fallbacks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reliability {
    /// Share of queries per non-ok state (rounded to 0.05; zeros omitted).
    pub states: BTreeMap<StateCode, f64>,
    /// Share of queries answered by the lexical fallback (rounded to 0.05).
    pub fallback_rate: Option<f64>,
}

/// Query kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KindCode {
    Request,
    Issue,
    Error,
    Conversational,
    Other,
}

impl KindCode {
    fn of(label: &str) -> KindCode {
        match label {
            "request" => KindCode::Request,
            "issue" => KindCode::Issue,
            "error" => KindCode::Error,
            "conversational" => KindCode::Conversational,
            _ => KindCode::Other,
        }
    }
}

/// How wn is used.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Usage {
    /// Share of queries per kind (rounded to 0.05; zeros omitted).
    pub kinds: BTreeMap<KindCode, f64>,
    /// Queries per week over the retention window.
    pub queries_per_week: Count,
    /// Share of answers that used the personal adapter (rounded to 0.05).
    pub adapter_rate: Option<f64>,
}

/// The whole report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReport {
    pub schema: u32,
    pub wn: WnBuild,
    pub setup: Setup,
    pub repos: Repos,
    pub performance: Performance,
    pub quality: Quality,
    pub reliability: Reliability,
    pub usage: Usage,
    /// True when detail was dropped to fit the issue URL.
    pub truncated: bool,
}

// ---------------------------------------------------------------------------------------------
// Collecting
// ---------------------------------------------------------------------------------------------

/// Machine facts, detected or injected (tests).
#[derive(Debug, Clone, PartialEq)]
pub struct System {
    pub os: Os,
    pub arch: Arch,
    pub threads: usize,
    pub ram_gb: Option<u64>,
    pub model: KnownModel,
    pub precision: Precision,
}

impl System {
    /// Detects this machine; `model_dir` is the resolved model (or `None` for the fallback).
    pub fn detect(model_dir: Option<&Path>) -> System {
        let os = match std::env::consts::OS {
            "macos" => Os::Macos,
            "linux" => Os::Linux,
            "windows" => Os::Windows,
            _ => Os::Other,
        };
        let arch = match std::env::consts::ARCH {
            "aarch64" => Arch::Aarch64,
            "x86_64" => Arch::X86_64,
            _ => Arch::Other,
        };
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let (model, precision) = match model_dir {
            Some(dir) if dir.join("wn-model.json").is_file() => {
                let name = dir
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let precision = if dir.join("model.onnx").is_file() {
                    Precision::Fp32
                } else if dir.join("model.q8.onnx").is_file() {
                    Precision::Q8
                } else {
                    Precision::None
                };
                (KnownModel::from_name(&name), precision)
            }
            _ => (KnownModel::Lexical, Precision::None),
        };
        System {
            os,
            arch,
            threads,
            ram_gb: ram_gb(),
            model,
            precision,
        }
    }
}

fn ram_gb() -> Option<u64> {
    if cfg!(target_os = "macos") {
        let out = Process::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .ok()?;
        let bytes: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
        Some(bytes / (1 << 30))
    } else if cfg!(target_os = "linux") {
        let text = std::fs::read_to_string("/proc/meminfo").ok()?;
        let kb: u64 = text
            .lines()
            .find(|l| l.starts_with("MemTotal:"))?
            .split_whitespace()
            .nth(1)?
            .parse()
            .ok()?;
        Some(kb / (1 << 20))
    } else {
        None
    }
}

/// Rounds a share to the nearest 0.05.
pub fn rate(x: f64) -> f64 {
    ((x * 20.0).round() / 20.0).clamp(0.0, 1.0)
}

/// Rounds a hit rate to 0.01.
pub fn hit(x: f64) -> f64 {
    ((x * 100.0).round() / 100.0).clamp(0.0, 1.0)
}

/// Rounds a latency: 1 ms under 20, 5 ms under 100, 10 ms under 1000, then 100 ms.
pub fn round_ms(ms: u64) -> u32 {
    let step = match ms {
        0..=19 => 1,
        20..=99 => 5,
        100..=999 => 10,
        _ => 100,
    };
    u32::try_from((ms + step / 2) / step * step).unwrap_or(u32::MAX)
}

fn percentile(sorted: &[u64], p: f64) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    Some(sorted[i.min(sorted.len() - 1)])
}

fn share<K: Ord + Copy>(counts: &BTreeMap<K, usize>, total: usize) -> BTreeMap<K, f64> {
    counts
        .iter()
        .filter_map(|(k, &n)| {
            let r = rate(n as f64 / total.max(1) as f64);
            (r > 0.0).then_some((*k, r))
        })
        .collect()
}

/// Paths changed in commits between `from` and `from + window`, plus working-tree changes when
/// the window is still open. Local only.
pub type EditedFn<'a> = dyn Fn(&Path, u64, u64) -> HashSet<String> + 'a;

/// [`EditedFn`] backed by git.
pub fn git_edited(root: &Path, from: u64, now: u64) -> HashSet<String> {
    let mut out = HashSet::new();
    let until = from + USEFUL_WINDOW_S;
    if let Ok(o) = Process::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "log",
            &format!("--since=@{from}"),
            &format!("--until=@{until}"),
            "--name-only",
            "--format=",
        ])
        .output()
    {
        out.extend(
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .filter(|l| !l.is_empty())
                .map(str::to_string),
        );
    }
    if now < until {
        if let Ok(o) = Process::new("git")
            .arg("-C")
            .arg(root)
            .args(["status", "--porcelain", "--untracked-files=no"])
            .output()
        {
            out.extend(
                String::from_utf8_lossy(&o.stdout)
                    .lines()
                    .filter_map(|l| l.get(3..))
                    .map(|p| p.rsplit(" -> ").next().unwrap_or(p).to_string()),
            );
        }
    }
    out
}

fn build() -> WnBuild {
    WnBuild {
        version: Version::try_from(env!("CARGO_PKG_VERSION").to_string())
            .unwrap_or_else(|_| Version("0".into())),
        // Set by build.rs (full sha, or empty when not built from git).
        commit: option_env!("WN_GIT_COMMIT")
            .and_then(|c| c.get(..7))
            .and_then(|c| BuildCommit::try_from(c.to_lowercase()).ok()),
    }
}

/// Builds the report from logged usage. `edited` answers "which paths changed after this
/// answer" (git in production, a stub in tests).
pub fn collect(usage: &[RepoUsage], system: &System, now: u64, edited: &EditedFn) -> UsageReport {
    let threads = match system.threads {
        0..=4 => Threads::UpTo4,
        5..=8 => Threads::UpTo8,
        9..=16 => Threads::UpTo16,
        _ => Threads::More,
    };
    let ram = match system.ram_gb {
        None => Ram::Unknown,
        Some(0..=7) => Ram::Under8,
        Some(8..=15) => Ram::To16,
        Some(16..=31) => Ram::To32,
        Some(32..=63) => Ram::To64,
        Some(_) => Ram::More,
    };

    let mut sizes: BTreeMap<Files, usize> = BTreeMap::new();
    let mut history: BTreeMap<History, usize> = BTreeMap::new();
    let mut lang: BTreeMap<Language, usize> = BTreeMap::new();
    let mut index_ms: Vec<u64> = Vec::new();
    let mut peak: Option<u64> = None;
    let mut bench: Vec<BenchSummary> = Vec::new();
    for repo in usage {
        let files = repo
            .index
            .as_ref()
            .map(|i| i.files)
            .or_else(|| repo.queries.last().map(|q| q.files));
        if let Some(n) = files {
            *sizes.entry(Files::of(n)).or_default() += 1;
        }
        if let Some(ix) = &repo.index {
            *history.entry(History::of(ix.history_commits)).or_default() += 1;
            for (ext, n) in &ix.extensions {
                *lang.entry(Language::of_extension(ext)).or_default() += n;
            }
            index_ms.push(ix.ms);
            if let Some(mb) = ix.peak_mb {
                peak = Some(peak.map_or(mb, |p| p.max(mb)));
            }
        }
        if let Some(b) = &repo.bench {
            bench.push(BenchSummary {
                files: Files::of(b.files_median),
                model: KnownModel::from_name(&b.model),
                lexical: b.lexical.map(hit),
                model_hits: b.model_hits.map(hit),
                adapter_hits: b.adapter_hits.map(|h| h.map(hit)),
            });
        }
    }
    let lang_total: usize = lang.values().sum();
    let languages: BTreeMap<Language, u8> = lang
        .iter()
        .filter_map(|(l, &n)| {
            let pct = ((n as f64 / lang_total.max(1) as f64) * 10.0).round() as u8 * 10;
            (pct >= 10).then_some((*l, pct))
        })
        .collect();
    index_ms.sort_unstable();
    bench.sort_by(|a, b| {
        (a.files, a.model)
            .cmp(&(b.files, b.model))
            .then(a.model_hits[1].total_cmp(&b.model_hits[1]))
    });
    bench.truncate(MAX_BENCH);

    let queries: Vec<&wn_daemon::usage::QueryEvent> =
        usage.iter().flat_map(|r| r.queries.iter()).collect();
    let n = queries.len();
    let mut ms: Vec<u64> = queries.iter().map(|q| q.ms).collect();
    ms.sort_unstable();
    let mut states: BTreeMap<StateCode, usize> = BTreeMap::new();
    let mut kinds: BTreeMap<KindCode, usize> = BTreeMap::new();
    let (mut fallback, mut adapted) = (0usize, 0usize);
    for q in &queries {
        if let Some(s) = StateCode::of(&q.state) {
            *states.entry(s).or_default() += 1;
        }
        *kinds.entry(KindCode::of(&q.kind)).or_default() += 1;
        fallback += usize::from(q.model.starts_with("hash-"));
        adapted += usize::from(q.adapter);
    }
    let abstained = states.get(&StateCode::Abstain).copied().unwrap_or(0);

    // Hint usefulness: did any hinted file change within a day of the answer?
    let (mut checked, mut useful) = (0usize, 0usize);
    for repo in usage {
        let Some(root) = &repo.root else { continue };
        let answered: Vec<_> = repo
            .queries
            .iter()
            .filter(|q| q.state == "ok" && !q.hinted.is_empty())
            .collect();
        for q in answered.iter().rev().take(MAX_CHECKED) {
            let changed = edited(root, q.ts, now);
            checked += 1;
            useful += usize::from(q.hinted.iter().any(|h| changed.contains(h)));
        }
    }

    let oldest = queries.iter().map(|q| q.ts).min().unwrap_or(now);
    let weeks = ((now.saturating_sub(oldest)) as f64 / (7.0 * 86_400.0)).max(1.0);
    UsageReport {
        schema: SCHEMA,
        wn: build(),
        setup: Setup {
            os: system.os,
            arch: system.arch,
            threads,
            ram,
            model: system.model,
            precision: system.precision,
        },
        repos: Repos {
            count: Count::of(usage.len()),
            sizes: sizes.into_iter().map(|(k, v)| (k, Count::of(v))).collect(),
            languages,
            history: history
                .into_iter()
                .map(|(k, v)| (k, Count::of(v)))
                .collect(),
        },
        performance: Performance {
            index_time: percentile(&index_ms, 0.5).map(IndexTime::of_ms),
            query_ms_p50: percentile(&ms, 0.5).map(round_ms),
            query_ms_p95: percentile(&ms, 0.95).map(round_ms),
            peak_memory: peak.map(Memory::of_mb),
        },
        quality: Quality {
            bench,
            abstain_rate: (n > 0).then(|| rate(abstained as f64 / n as f64)),
            hint_usefulness: (checked > 0).then(|| rate(useful as f64 / checked as f64)),
            hints_checked: Count::of(checked),
        },
        reliability: Reliability {
            states: share(&states, n),
            fallback_rate: (n > 0).then(|| rate(fallback as f64 / n as f64)),
        },
        usage: Usage {
            kinds: share(&kinds, n),
            queries_per_week: Count::of((n as f64 / weeks).round() as usize),
            adapter_rate: (n > 0).then(|| rate(adapted as f64 / n as f64)),
        },
        truncated: false,
    }
}

/// Collects from the usage log under `cache_home`.
pub fn collect_from(cache_home: &Path, system: &System, now: u64) -> UsageReport {
    let usage = load_all(cache_home, now);
    collect(&usage, system, now, &git_edited)
}

// ---------------------------------------------------------------------------------------------
// Rendering and the issue URL
// ---------------------------------------------------------------------------------------------

/// The JSON block posted in the issue.
pub fn to_json(report: &UsageReport) -> String {
    serde_json::to_string_pretty(report).unwrap_or_default()
}

fn opt<T: std::fmt::Display>(v: Option<T>) -> String {
    v.map_or_else(|| "–".to_string(), |v| v.to_string())
}

fn slug<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

fn map_line<K: Serialize, V: std::fmt::Display>(m: &BTreeMap<K, V>) -> String {
    if m.is_empty() {
        return "–".into();
    }
    m.iter()
        .map(|(k, v)| format!("{} {v}", slug(k)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn hits(h: &[f64; 3]) -> String {
    format!("{:.2} / {:.2} / {:.2}", h[0], h[1], h[2])
}

/// Human-readable summary plus the exact JSON that will be posted.
pub fn render_markdown(report: &UsageReport) -> String {
    let r = report;
    let mut s = String::new();
    s.push_str(MARKER);
    s.push_str("\n## wn usage report\n\n");
    s.push_str(&format!(
        "- wn {} ({})\n- setup: {} {}, {} threads, {} RAM, model {} ({})\n",
        String::from(r.wn.version.clone()),
        r.wn.commit
            .clone()
            .map_or("unknown build".into(), String::from),
        slug(&r.setup.os),
        slug(&r.setup.arch),
        slug(&r.setup.threads),
        slug(&r.setup.ram),
        slug(&r.setup.model),
        slug(&r.setup.precision),
    ));
    s.push_str(&format!(
        "- repositories: {} (sizes: {}; adapter history: {})\n- languages (% of files): {}\n",
        slug(&r.repos.count),
        map_line(
            &r.repos
                .sizes
                .iter()
                .map(|(k, v)| (*k, slug(v)))
                .collect::<BTreeMap<_, _>>()
        ),
        map_line(
            &r.repos
                .history
                .iter()
                .map(|(k, v)| (*k, slug(v)))
                .collect::<BTreeMap<_, _>>()
        ),
        map_line(&r.repos.languages),
    ));
    s.push_str(&format!(
        "- speed: index {}, query p50 {} ms / p95 {} ms, peak memory {}\n",
        r.performance.index_time.map_or("–".into(), |t| slug(&t)),
        opt(r.performance.query_ms_p50),
        opt(r.performance.query_ms_p95),
        r.performance.peak_memory.map_or("–".into(), |m| slug(&m)),
    ));
    s.push_str(&format!(
        "- quality: abstain rate {}, hints later edited {} (of {} answers checked)\n",
        opt(r.quality.abstain_rate),
        opt(r.quality.hint_usefulness),
        slug(&r.quality.hints_checked),
    ));
    for b in &r.quality.bench {
        s.push_str(&format!(
            "  - bench ({} files, {}): hit@1/3/10 lexical {}, model {}, + adapter {}\n",
            slug(&b.files),
            slug(&b.model),
            hits(&b.lexical),
            hits(&b.model_hits),
            b.adapter_hits.as_ref().map_or("–".into(), hits),
        ));
    }
    s.push_str(&format!(
        "- reliability: {}; lexical fallback {}\n- usage: {} queries/week; kinds {}; adapter {}\n",
        map_line(&r.reliability.states),
        opt(r.reliability.fallback_rate),
        slug(&r.usage.queries_per_week),
        map_line(&r.usage.kinds),
        opt(r.usage.adapter_rate),
    ));
    if r.truncated {
        s.push_str("- note: some detail was dropped to fit the issue link\n");
    }
    s.push_str("\n```json\n");
    s.push_str(&to_json(r));
    s.push_str("\n```\n");
    s
}

/// Percent-encodes everything except RFC 3986 unreserved characters.
pub fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The pre-filled issue-form URL. When it would exceed [`MAX_URL`], bench detail and then the
/// language mix are dropped and `truncated` is set; returns the URL and the report it carries.
pub fn issue_url(report: &UsageReport) -> (String, UsageReport) {
    issue_url_within(report, MAX_URL)
}

/// [`issue_url`] with an explicit length limit.
pub fn issue_url_within(report: &UsageReport, max: usize) -> (String, UsageReport) {
    let make = |r: &UsageReport| {
        format!(
            "https://github.com/{REPO}/issues/new?template=usage-report.yml&labels=usage-report&title={}&report={}",
            url_encode(TITLE),
            url_encode(&serde_json::to_string(r).unwrap_or_default()),
        )
    };
    let mut r = report.clone();
    let mut url = make(&r);
    while url.len() > max && !r.quality.bench.is_empty() {
        r.quality.bench.pop();
        r.truncated = true;
        url = make(&r);
    }
    if url.len() > max {
        r.repos.languages.clear();
        r.truncated = true;
        url = make(&r);
    }
    (url, r)
}

// ---------------------------------------------------------------------------------------------
// The flow: a state machine
// ---------------------------------------------------------------------------------------------

/// Where the report flow is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportState {
    Collecting,
    Previewing,
    Confirmed,
    Cancelled,
    Posting,
    Posted,
    Failed,
}

/// What happened.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportEvent {
    Collected,
    CollectFailed,
    Accept,
    Decline,
    Post,
    PostOk,
    PostFailed,
}

/// Every state.
pub const STATES: [ReportState; 7] = [
    ReportState::Collecting,
    ReportState::Previewing,
    ReportState::Confirmed,
    ReportState::Cancelled,
    ReportState::Posting,
    ReportState::Posted,
    ReportState::Failed,
];

/// Every event.
pub const EVENTS: [ReportEvent; 7] = [
    ReportEvent::Collected,
    ReportEvent::CollectFailed,
    ReportEvent::Accept,
    ReportEvent::Decline,
    ReportEvent::Post,
    ReportEvent::PostOk,
    ReportEvent::PostFailed,
];

/// The legal transitions; everything else is rejected and leaves the state unchanged.
pub const TABLE: &[(ReportState, ReportEvent, ReportState)] = &[
    (
        ReportState::Collecting,
        ReportEvent::Collected,
        ReportState::Previewing,
    ),
    (
        ReportState::Collecting,
        ReportEvent::CollectFailed,
        ReportState::Failed,
    ),
    (
        ReportState::Previewing,
        ReportEvent::Accept,
        ReportState::Confirmed,
    ),
    (
        ReportState::Previewing,
        ReportEvent::Decline,
        ReportState::Cancelled,
    ),
    (
        ReportState::Confirmed,
        ReportEvent::Post,
        ReportState::Posting,
    ),
    (
        ReportState::Posting,
        ReportEvent::PostOk,
        ReportState::Posted,
    ),
    (
        ReportState::Posting,
        ReportEvent::PostFailed,
        ReportState::Failed,
    ),
];

/// The report flow machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReportFlow {
    state: ReportState,
}

impl Default for ReportFlow {
    fn default() -> Self {
        ReportFlow {
            state: ReportState::Collecting,
        }
    }
}

impl ReportFlow {
    pub fn state(&self) -> ReportState {
        self.state
    }

    /// Applies an event; illegal events are rejected and change nothing.
    pub fn handle(
        &mut self,
        event: ReportEvent,
    ) -> Result<ReportState, Illegal<ReportState, ReportEvent>> {
        self.state = step(TABLE, self.state, event)?;
        Ok(self.state)
    }

    /// Whether anything can leave the machine in this state (only after an explicit Accept).
    pub fn may_send(&self) -> bool {
        matches!(self.state, ReportState::Confirmed | ReportState::Posting)
    }
}

/// Interaction and posting, injectable for tests.
pub trait ReportIo {
    /// Shows text to the user.
    fn show(&mut self, text: &str);
    /// Asks a yes/no question; `false` when there is no terminal.
    fn confirm(&mut self, prompt: &str) -> bool;
    /// Posts the report; returns where it went (issue URL or "opened in browser").
    fn post(&mut self, url: &str, title: &str, body: &str) -> Result<String, String>;
}

/// `wn report` options.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReportOptions {
    /// Print the report JSON only; never post.
    pub json: bool,
    /// Show the preview and the link; never post.
    pub dry_run: bool,
}

/// Runs the flow over an already-collected report. Returns the final state.
pub fn run_flow(report: &UsageReport, opts: ReportOptions, io: &mut dyn ReportIo) -> ReportState {
    let mut flow = ReportFlow::default();
    let _ = flow.handle(ReportEvent::Collected);
    let (url, report) = issue_url(report);
    if opts.json {
        io.show(&to_json(&report));
        let _ = flow.handle(ReportEvent::Decline);
        return flow.state();
    }
    io.show(&render_markdown(&report));
    if opts.dry_run {
        io.show(&format!(
            "dry run: nothing posted. The issue link would be:\n{url}"
        ));
        let _ = flow.handle(ReportEvent::Decline);
        return flow.state();
    }
    let accepted = io.confirm(&format!(
        "The text above is exactly what will be posted as a public issue on github.com/{REPO}.\nPost it? [y/N] "
    ));
    if !accepted {
        let _ = flow.handle(ReportEvent::Decline);
        io.show("Not posted.");
        return flow.state();
    }
    let _ = flow.handle(ReportEvent::Accept);
    let _ = flow.handle(ReportEvent::Post);
    debug_assert!(flow.may_send());
    match io.post(&url, TITLE, &render_markdown(&report)) {
        Ok(where_) => {
            let _ = flow.handle(ReportEvent::PostOk);
            io.show(&format!("Thanks! {where_}"));
        }
        Err(e) => {
            let _ = flow.handle(ReportEvent::PostFailed);
            io.show(&format!(
                "Could not post ({e}). You can open this link yourself:\n{url}"
            ));
        }
    }
    flow.state()
}

/// The real terminal, browser and `gh`.
pub struct TerminalIo;

impl ReportIo for TerminalIo {
    fn show(&mut self, text: &str) {
        println!("{text}");
    }

    fn confirm(&mut self, prompt: &str) -> bool {
        use std::io::{BufRead as _, IsTerminal as _, Write as _};
        if !std::io::stdin().is_terminal() {
            println!("{prompt}\n(no terminal: not posting)");
            return false;
        }
        print!("{prompt}");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
    }

    fn post(&mut self, url: &str, title: &str, body: &str) -> Result<String, String> {
        let gh_ok = Process::new("gh")
            .args(["auth", "status"])
            .output()
            .is_ok_and(|o| o.status.success());
        if gh_ok {
            let out = Process::new("gh")
                .args([
                    "issue", "create", "--repo", REPO, "--title", title, "--body", body,
                ])
                .output()
                .map_err(|e| e.to_string())?;
            if out.status.success() {
                return Ok(format!(
                    "Issue created: {}",
                    String::from_utf8_lossy(&out.stdout).trim()
                ));
            }
        }
        let opener = if cfg!(target_os = "macos") {
            ("open", vec![url])
        } else if cfg!(target_os = "windows") {
            ("cmd", vec!["/C", "start", "", url])
        } else {
            ("xdg-open", vec![url])
        };
        let status = Process::new(opener.0)
            .args(&opener.1)
            .status()
            .map_err(|e| e.to_string())?;
        if status.success() {
            Ok("Opened the pre-filled issue in your browser; submit it there.".into())
        } else {
            Err(format!("{} exited with {status}", opener.0))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Maintainer side: validate posted reports and summarise them (used by the GitHub Action)
// ---------------------------------------------------------------------------------------------

/// Parses and validates a posted report: unknown fields, free text, out-of-range numbers and
/// unknown enum values are all rejected.
pub fn validate(json: &str) -> Result<UsageReport, String> {
    let r: UsageReport = serde_json::from_str(json).map_err(|e| e.to_string())?;
    if r.schema != SCHEMA {
        return Err(format!("unsupported schema {}", r.schema));
    }
    let unit = |x: f64| (0.0..=1.0).contains(&x);
    let rates = [
        r.quality.abstain_rate,
        r.quality.hint_usefulness,
        r.reliability.fallback_rate,
        r.usage.adapter_rate,
    ];
    let ok = rates.iter().flatten().all(|x| unit(*x))
        && r.reliability.states.values().all(|x| unit(*x))
        && r.usage.kinds.values().all(|x| unit(*x))
        && r.repos.languages.values().all(|p| *p <= 100 && p % 10 == 0)
        && r.quality.bench.len() <= MAX_BENCH
        && r.quality.bench.iter().all(|b| {
            b.lexical.iter().chain(&b.model_hits).all(|x| unit(*x))
                && b.adapter_hits.iter().flatten().all(|x| unit(*x))
        })
        && r.performance.query_ms_p50.unwrap_or(0) <= 600_000
        && r.performance.query_ms_p95.unwrap_or(0) <= 600_000;
    ok.then_some(r)
        .ok_or_else(|| "a value is out of range".to_string())
}

/// Extracts the first fenced ```json block from an issue body.
pub fn extract_json(body: &str) -> Option<&str> {
    let start = body.find("```json")? + "```json".len();
    let rest = &body[start..];
    let end = rest.find("```")?;
    Some(rest[..end].trim())
}

/// hit@3 samples per (size, language): lexical, model, model + adapter.
type HitGroups = BTreeMap<(Files, Language), [Vec<f64>; 3]>;
/// Latency samples per (os, arch, threads): p50s and p95s.
type LatencyGroups = BTreeMap<(String, String, String), [Vec<f64>; 2]>;

fn median(mut v: Vec<f64>) -> Option<f64> {
    if v.is_empty() {
        return None;
    }
    v.sort_by(f64::total_cmp);
    Some(v[v.len() / 2])
}

fn fmt2(x: Option<f64>) -> String {
    x.map_or_else(|| "–".into(), |x| format!("{x:.2}"))
}

/// Markdown summary of many reports: hit@3 by repository size and language, latency by
/// hardware, abstain rate and hint usefulness.
pub fn summarize(reports: &[UsageReport]) -> String {
    let mut s = format!("# wn usage reports\n\n{} reports.\n\n", reports.len());
    s.push_str("## hit@3 from `wn bench --history` (median)\n\n| repo size | language | runs | lexical | model | + adapter |\n|---|---|---|---|---|---|\n");
    let mut groups: HitGroups = BTreeMap::new();
    for r in reports {
        let lang = r
            .repos
            .languages
            .iter()
            .max_by_key(|(_, p)| **p)
            .map_or(Language::Other, |(l, _)| *l);
        for b in &r.quality.bench {
            let g = groups.entry((b.files, lang)).or_default();
            g[0].push(b.lexical[1]);
            g[1].push(b.model_hits[1]);
            if let Some(a) = b.adapter_hits {
                g[2].push(a[1]);
            }
        }
    }
    for ((files, lang), [lex, model, ad]) in groups {
        s.push_str(&format!(
            "| {} | {} | {} | {} | {} | {} |\n",
            slug(&files),
            slug(&lang),
            model.len(),
            fmt2(median(lex)),
            fmt2(median(model)),
            fmt2(median(ad)),
        ));
    }
    s.push_str("\n## Latency by hardware (median of reports)\n\n| os | arch | threads | reports | p50 ms | p95 ms |\n|---|---|---|---|---|---|\n");
    let mut hw: LatencyGroups = BTreeMap::new();
    for r in reports {
        let key = (
            slug(&r.setup.os),
            slug(&r.setup.arch),
            slug(&r.setup.threads),
        );
        let g = hw.entry(key).or_default();
        if let Some(p) = r.performance.query_ms_p50 {
            g[0].push(f64::from(p));
        }
        if let Some(p) = r.performance.query_ms_p95 {
            g[1].push(f64::from(p));
        }
    }
    for ((os, arch, threads), [p50, p95]) in hw {
        let n = p50.len();
        let f = |v: Vec<f64>| median(v).map_or_else(|| "–".into(), |x| format!("{x:.0}"));
        s.push_str(&format!(
            "| {os} | {arch} | {threads} | {n} | {} | {} |\n",
            f(p50),
            f(p95)
        ));
    }
    let abstain: Vec<f64> = reports
        .iter()
        .filter_map(|r| r.quality.abstain_rate)
        .collect();
    let useful: Vec<f64> = reports
        .iter()
        .filter_map(|r| r.quality.hint_usefulness)
        .collect();
    s.push_str(&format!(
        "\n## Answers\n\n- abstain rate (median): {}\n- hints later edited (median): {}\n",
        fmt2(median(abstain)),
        fmt2(median(useful)),
    ));
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn buckets_and_rounding() {
        assert_eq!(Files::of(999), Files::Under1k);
        assert_eq!(Files::of(1000), Files::To3k);
        assert_eq!(Files::of(10_000), Files::More);
        assert_eq!(Count::of(0), Count::Zero);
        assert_eq!(Count::of(200), Count::Lots);
        assert_eq!(rate(0.126), 0.15);
        assert_eq!(hit(0.8149), 0.81);
        assert_eq!(round_ms(17), 17);
        assert_eq!(round_ms(23), 25);
        assert_eq!(round_ms(1234), 1200);
        assert_eq!(
            KnownModel::from_name("gemma-xl1-30ae960f"),
            KnownModel::GemmaXl1
        );
        assert_eq!(KnownModel::from_name("hash-bow2-512"), KnownModel::Lexical);
        assert_eq!(KnownModel::from_name("my-model"), KnownModel::Custom);
        assert_eq!(Language::of_extension("TSX"), Language::Typescript);
    }

    #[test]
    fn validated_strings_reject_free_text() {
        assert!(Version::try_from("0.0.1".to_string()).is_ok());
        assert!(Version::try_from("0.0.1-evil/path".to_string()).is_err());
        assert!(BuildCommit::try_from("abc1234".to_string()).is_ok());
        assert!(BuildCommit::try_from("not-a-sha".to_string()).is_err());
    }
}
